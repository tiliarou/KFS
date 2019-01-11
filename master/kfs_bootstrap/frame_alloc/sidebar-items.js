initSidebarItems({"constant":[["FRAMES_BITMAP_SIZE","The size of the frames_bitmap (~128ko)"],["FRAME_BASE_LOG",""],["FRAME_BASE_MASK",""],["FRAME_FREE",""],["FRAME_OCCUPIED",""],["FRAME_OFFSET_MASK",""],["MEMORY_FRAME_SIZE","A memory frame is the same size as a page"]],"fn":[["addr_to_frame","Gets the frame number from a physical address"],["frame_to_addr","Gets the physical address from a frame number"],["round_to_page","Rounds an address to its page address"],["round_to_page_upper","Rounds an address to the next page address except if its offset in that page is 0"]],"static":[["FRAMES_BITMAP","A big bitmap denoting for every frame if it is free or not"]],"struct":[["AllocatorBitmap","A big bitmap denoting for every frame if it is free or not"],["Frame","A pointer to a physical frame"],["FrameAllocator","A physical memory manger to allocate and free memory frames"]]});