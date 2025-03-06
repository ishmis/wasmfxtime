/// wapper around a handler name (i.e. a vmcontref)
pub mod safe_vm_handlerobj {
    use crate::runtime::vm::continuation::imp::VMContRef;
    use core::ptr::NonNull;

    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct VMHandlerObj(NonNull<VMContRef>);

    impl VMHandlerObj {
        pub fn new(contref: NonNull<VMContRef>) -> Self {
            Self(contref)
        }
    }
}

pub use safe_vm_handlerobj::*;

unsafe impl Send for VMHandlerObj {}
unsafe impl Sync for VMHandlerObj {}
