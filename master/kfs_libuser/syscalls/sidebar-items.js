initSidebarItems({"fn":[["accept_session","Accept a connection on the given port."],["close_handle","Close the given handle."],["connect_to_named_port","Creates a session to the given named port."],["connect_to_port","Connects to the given named port."],["create_interrupt_event","Create a waitable object for the given IRQ number."],["create_port","Creates an anonymous port."],["create_session","Create an anonymous session."],["create_shared_memory","Creates a shared memory handle."],["create_thread","Creates a thread in the current process."],["exit_process","Exits the process, killing all threads."],["exit_thread","Exits the current thread."],["manage_named_port","Creates a named port."],["map_framebuffer","Maps the framebuffer to a kernel-chosen address."],["map_mmio_region","Maps a physical region in the address space of the process."],["map_shared_memory","Maps a shared memory."],["output_debug_string","Print the given string to the kernel's debug output."],["query_memory","Query information about an address. Will fetch the page-aligned mapping `addr` falls in. mapping that contains the provided address."],["query_physical_address","Gets the physical region a given virtual address maps."],["reply_and_receive_with_user_buffer","Reply and Receive IPC requests on the given handles."],["send_sync_request_with_user_buffer","Send an IPC request through the given pipe."],["set_heap_size","Resize the heap of a process, just like a brk. It can both expand, and shrink the heap."],["sleep_thread","Sleeps for a specified amount of time, or yield thread."],["start_thread","Starts the thread for the provided handle."],["syscall","Generic syscall function."],["syscall_inner",""],["unmap_shared_memory","Unmaps a shared memory."],["wait_synchronization","Wait for an event on the given handles."]],"mod":[["nr","Syscall numbers"],["syscall_inner",""]],"struct":[["MemoryInfo","The structure returned by the `query_memory` syscall."],["MemoryPermissions","Memory permissions of a memory area."],["Registers","Register backup structure. The syscall_inner will pop the registers from this structure before jumping into the kernel, and then update the structure with the registers set by the syscall."]]});