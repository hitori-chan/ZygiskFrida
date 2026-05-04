#include "remapper.h"

#include <link.h>
#include <sys/mman.h>

#include <cerrno>
#include <cinttypes>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

#include "log.h"

// Struct to hold a single entry in /proc/maps/
// Format: 7ac49c2000(start)-7ac4a26000(end) r--p (permissions) 00000000(offset) 00:00 0 (dev) 1245 (inode) /apex/com.android.runtime/bin/linker64 (path) // NOLINT
struct PROCMAPSINFO {
    uintptr_t start, end, offset;
    uint8_t perms;
    ino_t inode;
    std::string dev;
    std::string path;
};


std::vector<PROCMAPSINFO> get_modules_by_name(std::string const &mName) {
    std::vector<PROCMAPSINFO> maps;

    char buffer[512];
    FILE *fp = fopen("/proc/self/maps", "re");

    if (fp == nullptr) {
        return maps;
    }

    while (fgets(buffer, sizeof(buffer), fp)) {
        if (strstr(buffer, mName.c_str())) {
            PROCMAPSINFO info{};
            char perms[10];
            char path[255];
            char dev[25];

            int matched = sscanf(
                buffer,
                "%" SCNxPTR "-%" SCNxPTR " %s %" SCNxPTR " %s %ld %s",
                &info.start, &info.end, perms, &info.offset, dev, &info.inode, path);
            if (matched < 7) {
                continue;
            }

            /* Store process permissions in the struct directly via bitwise operations */
            if (strchr(perms, 'r')) info.perms |= PROT_READ;
            if (strchr(perms, 'w')) info.perms |= PROT_WRITE;
            if (strchr(perms, 'x')) info.perms |= PROT_EXEC;

            info.dev = dev;
            info.path = path;

            maps.push_back(info);
        }
    }

    fclose(fp);

    return maps;
}

void remap_lib(std::string const &lib_path) {
    std::string lib_name = lib_path.substr(lib_path.find_last_of("/\\") + 1);

    std::vector<PROCMAPSINFO> maps = get_modules_by_name(lib_name);
    if (maps.empty()) {
        return;
    }

    LOGI("Remapping %s", lib_name.c_str());

    for (PROCMAPSINFO const &info : maps) {
        void *address = reinterpret_cast<void *>(info.start);
        size_t size = info.end - info.start;

        void *map = mmap(0, size, PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
        if (map == MAP_FAILED) {
            LOGE("Failed to allocate remap memory: %s", strerror(errno));
            return;
        }

        if ((info.perms & PROT_READ) == 0) {
            LOGI("Removing memory protection: %s", info.path.c_str());
            if (mprotect(address, size, PROT_READ) != 0) {
                LOGE("Failed to remove memory protection: %s", strerror(errno));
                munmap(map, size);
                return;
            }
        }

        /* Copy the in-memory data to new virtual location via the memove, allocate and commit it via mremap */
        std::memmove(map, address, size);
        void *remapped = mremap(map, size, size, MREMAP_MAYMOVE | MREMAP_FIXED, info.start);
        if (remapped == MAP_FAILED) {
            LOGE("Failed to remap memory: %s", strerror(errno));
            munmap(map, size);
            return;
        }

        /* Re-apply memory protections */
        mprotect(reinterpret_cast<void *>(info.start), size, info.perms);

        LOGI("Allocated at address %p with size of %zu", map, size);
    }

    LOGI("Remapped");
}
