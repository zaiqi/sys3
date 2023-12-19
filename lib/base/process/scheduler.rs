pub static NUM_TASKS: AtomicU32 = AtomicU32::new(0);
pub static TID_COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct Scheduler {
   /// Task which is currently running.
   pub current_task: Rc<RefCell<Task>>,
   /// Currently idle task.
   pub idle_task: Rc<RefCell<Task>>,
   /// Queue of tasks, which are ready to run.
   pub ready_queue: SpinlockIrqSave<PriorityTaskQueue>,
   /// Queue of tasks, which are finished and can be released.
   pub finished_tasks: SpinlockIrqSave<VecDeque<TaskId>>,
   /// Map between task IDs and task control block.
   pub tasks: SpinlockIrqSave<BTreeMap<TaskId, Rc<RefCell<Task>>>>,
}

impl Scheduler {
   pub fn new() -> Scheduler {
      let tid = TaskId::from(TID_COUNTER.fetch_add(1, Ordering::SeqCst) as u64);
      let idle_task = Rc::new(RefCell::new(Task::new_idle(tid)));
      let tasks = SpinlockIrqSave::new(BTreeMap::new());

      tasks.lock().insert(tid, idle_task.clone());

      return Scheduler{
         current_task: idle_task.clone(),
         idle_task: idle_task.clone(),
         ready_queue: SpinlockIrqSave::new(PriorityTaskQueue::new()),
         finished_tasks: SpinlockIrqSave::new(VecDeque::<TaskId>::new()),
         tasks: tasks,
      };
   }

   fn get_tid(&self) -> TaskId {
      loop {
         let id = TaskId::from(TID_COUNTER.fetch_add(1, Ordering::SeqCst) as u64);

         if self.tasks.lock().contains_key(&id) == false {
            return id;
         }
      }
   }

   pub fn spawn(&mut self, func: extern "C" fn(), priority: TaskPriority) -> Result<TaskId, ProcError> {
      let closure = || {
         let prio_number = priority.into() as usize;

         if prio_number >= NUM_PRIORITIES {
            return Err(ProcError::BadPriority);
         }

         // Create the new task.
         let tid = self.get_tid();
         let task = Rc::new(RefCell::new(Task::new(tid, TaskStatus::Ready, priority)));

         task.borrow_mut().create_stack_frame(func);

         // Add it to the task lists.
         self.ready_queue.lock().push(task.clone());
         self.tasks.lock().insert(tid, task);
         NUM_TASKS.fetch_add(1, Ordering::SeqCst);

         log::info!("Creating task {}", tid);

         Ok(tid)
      };

      return irqsave(closure);
   }

   fn cleanup(&mut self) {
      // destroy user space
      //drop_user_space();

      self.current_task.borrow_mut().status = TaskStatus::Finished;

      // update the number of tasks
      NUM_TASKS.fetch_sub(1, Ordering::SeqCst);
   }

   pub fn exit(&mut self) -> ! {
      let closure = || {
         if self.current_task.borrow().status != TaskStatus::Idle {
            log::info!("finish task with id {}", self.current_task.borrow().id);
            self.cleanup();
         } else {
            panic!("unable to terminate idle task");
         }
      };

      irqsave(closure);

      self.reschedule();

      // we should never reach this point
      panic!("exit failed!");
   }

   pub fn abort(&mut self) -> ! {
      let closure = || {
         if self.current_task.borrow().status != TaskStatus::Idle {
            log::info!("abort task with id {}", self.current_task.borrow().id);
            self.cleanup();
         } else {
            panic!("unable to terminate idle task");
         }
      };

      irqsave(closure);

      self.reschedule();

      // we should never reach this point
      panic!("abort failed!");
   }

   pub fn block_current_task(&mut self) -> Rc<RefCell<Task>> {
      let closure = || {
         if self.current_task.borrow().status == TaskStatus::Running {
            log::debug!("block task {}", self.current_task.borrow().id);

            self.current_task.borrow_mut().status = TaskStatus::Blocked;
            self.current_task.clone()
         } else {
            panic!("unable to block task {}", self.current_task.borrow().id);
         }
      };

      return irqsave(closure);
   }

   pub fn wakeup_task(&mut self, task: Rc<RefCell<Task>>) {
      let closure = || {
         if task.borrow().status == TaskStatus::Blocked {
            log::debug!("wakeup task {}", task.borrow().id);

            task.borrow_mut().status = TaskStatus::Ready;
            self.ready_queue.lock().push(task.clone());
         }
      };

      irqsave(closure);
   }

   pub fn get_current_taskid(&self) -> TaskId {
      irqsave(|| self.current_task.borrow().id)
   }

   /// Determines the start address of the stack
   #[no_mangle]
   pub fn get_current_stack(&self) -> usize {
      irqsave(|| (*self.current_task.borrow().stack).bottom())
   }

   pub fn get_root_page_table(&self) -> usize {
      self.current_task.borrow().root_page_table
   }

   pub fn set_root_page_table(&self, addr: usize) {
      self.current_task.borrow_mut().root_page_table = addr;
   }

   pub fn schedule(&mut self) {
      // do we have finished tasks? => drop tasks => deallocate implicitly the stack
      match self.finished_tasks.lock().pop_front() {
         Some(id) => {
            if self.tasks.lock().remove(&id).is_none() == true {
               log::info!("Unable to drop task {}", id);
            }
         }
         _ => {}
      }

      // Get information about the current task.
      let (current_id, current_stack_pointer, current_prio, current_status) = {
         let mut borrowed = self.current_task.borrow_mut();
         (
            borrowed.id,
            &mut borrowed.last_stack_pointer as *mut usize,
            borrowed.priority,
            borrowed.status,
         )
      };

      // do we have a task, which is ready?
      let mut next_task;
      if current_status == TaskStatus::Running {
         next_task = self.ready_queue.lock().pop_with_priority(current_prio);
      } else {
         next_task = self.ready_queue.lock().pop();
      }

      if next_task.is_none() == true {
         if current_status != TaskStatus::Running && current_status != TaskStatus::Idle {
            log::debug!("Switch to idle task");
            // current task isn't able to run and no other task available
            // => switch to the idle task
            next_task = Some(self.idle_task.clone());
         }
      }

      match next_task {
         Some(new_task) => {
            let (new_id, new_stack_pointer) = {
               let mut borrowed = new_task.borrow_mut();
               borrowed.status = TaskStatus::Running;
               (borrowed.id, borrowed.last_stack_pointer)
            };

            if current_status == TaskStatus::Running {
               log::debug!("Add task {} to ready queue", current_id);
               self.current_task.borrow_mut().status = TaskStatus::Ready;
               self.ready_queue.lock().push(self.current_task.clone());
            } else if current_status == TaskStatus::Finished {
               log::debug!("Task {} finished", current_id);
               self.current_task.borrow_mut().status = TaskStatus::Invalid;
               // release the task later, because the stack is required
               // to call the function "switch"
               // => push id to a queue and release the task later
               self.finished_tasks.lock().push_back(current_id);
            }

            log::debug!(
					"Switching task from {} to {} (stack {:#X} => {:#X})",
					current_id,
					new_id,
					unsafe { *current_stack_pointer },
					new_stack_pointer
				);

            self.current_task = new_task;

            unsafe {
               switch(current_stack_pointer, new_stack_pointer);
            }
         }
         _ => {}
      }
   }

   pub fn reschedule(&mut self) {
      irqsave(|| self.schedule());
   }
}

// IMPORTS //

use {
   crate::{
      arch::x86_64::kernel::{switch},
      external::*,
      process::{control::*, error::ProcError},
      sync::{irqsave, SpinlockIrqSave},
   },
   core::{
      cell::RefCell,
      sync::atomic::{AtomicU32, Ordering},
   },
   std_alloc::{
      collections::{BTreeMap, VecDeque},
      rc::Rc,
   },
};
