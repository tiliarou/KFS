initSidebarItems({"constant":[["PAGE_SIZE","The page size. Dictated by the MMU. In simple, elegant, sane i386 paging, a page is 4kB."]],"enum":[["PageState","A hierarchical paging is composed of entries. An entry can be in the following states:"]],"fn":[["read_cr2","Reads the value of cr2, retrieving the address that caused a page fault"]],"mod":[["arch","Arch-specific implementations of paging"],["bookkeeping","Bookkeeping of mappings in UserLand"],["cross_process","Cross Process Mapping"],["error","Errors specific to memory management"],["hierarchical_table","Arch-independent traits for architectures that implement paging as a hierarchy of page tables"],["kernel_memory","The management of kernel memory"],["lands","Module describing the split between the UserSpace and KernelSpace, and a few functions to work with it."],["mapping","Mapping"],["process_memory","The management of a process' memory"]],"struct":[["MappingAccessRights","The flags of a mapping."]]});