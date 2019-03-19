initSidebarItems({"enum":[["IPCBufferType","Type of an IPC Buffer. Depending on the type, the kernel will either map it in the remote process, or memcpy its content."],["MessageTy","Type of an IPC message."]],"fn":[["find_ty_cmdid","Quickly find the type and cmdid of an IPC message for the server dispatcher."]],"mod":[["buffer","Server wrappers around IPC Buffers"],["macros","IPC Macros"],["server","IPC Server primitives"]],"struct":[["HandleDescriptorHeader","Part of an HIPC command. Sent only when `MsgPackedHdr::enable_handle_descriptor` is true."],["IPCBuffer","An IPC Buffer represents a section of memory to send to the other side of the pipe. It is usually used for sending big chunks of data that would not send in the comparatively small argument area (which is usually around 200 bytes)."],["InBuffer","An incoming Buffer, also known as a Type-A Buffer."],["InPointer","An incoming Pointer buffer, also known as a Type-X Buffer."],["Message","A generic IPC message, representing either an IPC Request or an IPC Response."],["MsgPackedHdr","Represenens the header of an HIPC command."]]});