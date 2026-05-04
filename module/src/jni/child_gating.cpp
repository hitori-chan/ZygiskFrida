#include "child_gating.h"

#include <dobby.h>
#include <log.h>
#include <unistd.h>
#include <xdl.h>

#include <cstdlib>
#include <future>
#include <string>
#include <vector>

#include "config.h"
#include "inject.h"

static std::string child_gating_mode;  // NOLINT
static std::vector<std::string> injected_libraries;

static pid_t (*orig_fork)();

static pid_t (*orig_vfork)();

static pid_t handle_fork(pid_t (*original_fork)()) {
    pid_t parent_pid = getpid();
    LOGI("[child_gating][pid %d] detected fork/vfork", parent_pid);

    pid_t child_pid = original_fork();
    if (child_pid != 0) {
        LOGI("[child_gating][pid %d] returning from forking %d", parent_pid, child_pid);
        return child_pid;
    }

    child_pid = getpid();

    auto logContext = "[child_gating][pid " + std::to_string(child_pid) + "] ";

    if (child_gating_mode == "kill") {
        LOGI("%skilling child process", logContext.c_str());
        exit(0);
    }

    if (child_gating_mode == "freeze") {
        LOGI("%sfreezing child process",  logContext.c_str());
        std::promise<void>().get_future().wait();
        return 0;
    }

    if (child_gating_mode != "inject") {
        LOGI("%sunknown child_gating_mode %s",  logContext.c_str(), child_gating_mode.c_str());
        return 0;
    }

    for (auto &lib_path : injected_libraries) {
        LOGI("%sInjecting %s",  logContext.c_str(), lib_path.c_str());
        inject_lib(lib_path, logContext);
    }

    return 0;
}

pid_t fork_replacement() {
    return handle_fork(orig_fork);
}

pid_t vfork_replacement() {
    return handle_fork(orig_vfork);
}

static bool install_hook(void *address, void *replacement, void **original, char const *name) {
    if (address == nullptr) {
        LOGE("[child_gating] %s address not found", name);
        return false;
    }

    auto result = DobbyHook(address, replacement, original);
    if (result != 0) {
        LOGE("[child_gating] failed to install %s hook: %d", name, result);
        return false;
    }

    LOGI("[child_gating] %s hook installed", name);
    return true;
}

void enable_child_gating(child_gating_config const &cfg) {
    child_gating_mode = cfg.mode;
    injected_libraries = cfg.injected_libraries;

    LOGI("[child_gating] enabling child gating");

    void *forkAddr = dlsym(RTLD_DEFAULT, "fork");
    LOGI("[child_gating] fork address %p", forkAddr);
    void *vforkAddr = dlsym(RTLD_DEFAULT, "vfork");
    LOGI("[child_gating] vfork address %p", vforkAddr);

    bool fork_hooked = install_hook(
        forkAddr,
        reinterpret_cast<void *>(fork_replacement),
        reinterpret_cast<void **>(&orig_fork),
        "fork");
    bool vfork_hooked = install_hook(
        vforkAddr,
        reinterpret_cast<void *>(vfork_replacement),
        reinterpret_cast<void **>(&orig_vfork),
        "vfork");

    if (fork_hooked || vfork_hooked) {
        LOGI("[child_gating] child gating enabled");
    }
}
