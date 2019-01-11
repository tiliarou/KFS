initSidebarItems({"fn":[["eh_personality","The exception handling personality function for use in the bootstrap."],["find_free_address","Finds a free memory zone of the given size and alignment in the current process's virtual address space. Note that the address space is not reserved, a call to map_memory to that address space might fail if another thread maps to it first. It is recommended to use this function and the map syscall in a loop."],["main",""],["panic_fmt","Function called on `panic!` invocation. Prints the panic information to the kernel debug logger, and exits the process."],["rust_oom","OOM handler. Causes a panic."],["start","Executable entrypoint. Zeroes out the BSS, calls main, and finally exits the process."]],"macro":[["capabilities","Define the capabilities array in the .kernel_caps section. Has the following syntax:"],["object","Auto derive Object."]],"mod":[["__rg_allocator_abi",""],["allocator","Heap allocator."],["caps","Kernel Capabilities declaration"],["error","Error handling"],["io","The IO interface"],["ipc","Core IPC Routines"],["log_impl","Implementation for the log crate"],["sm","Service Manager"],["syscalls","Syscall Wrappers"],["terminal","Terminal rendering APIs"],["types","Core kernel types."],["vi","Vi Compositor service"],["window","Window creation and drawing APIs"]],"static":[["ALLOCATOR",""]],"trait":[["Termination",""]]});