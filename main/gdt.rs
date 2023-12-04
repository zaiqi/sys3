pub static DOUBLE_FAULT_IST_INDEX: u16 = 0;

pub static mut TSS: TaskStateSegment = {
   let mut tss = TaskStateSegment::new();
   tss
};

pub static mut GDT: GlobalDescriptorTable = {
   let mut gdt = GlobalDescriptorTable::new();
   gdt.add_entry(Descriptor::kernel_code_segment());
   gdt
};

pub fn initGDT() {
   unsafe {
      TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
         const STACK_SIZE: usize = 4096 * 5;
         static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

         let stack_start = VirtAddr::from_ptr(unsafe { &STACK });
         let stack_end = stack_start + STACK_SIZE;
         stack_end
      };

      GDT.add_entry(Descriptor::tss_segment(&TSS));

      GDT.load();
   }
}

// IMPORTS //

use x86_64::{
   VirtAddr,
   structures::{
      gdt::{GlobalDescriptorTable, Descriptor},
      tss::TaskStateSegment,
   },
};