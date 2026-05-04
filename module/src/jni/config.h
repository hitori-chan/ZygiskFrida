#ifndef ZYGISKFRIDA_CONFIG_H
#define ZYGISKFRIDA_CONFIG_H

#include <cstdint>
#include <string>
#include <vector>
#include <optional>

struct child_gating_config {
    bool enabled;
    std::string mode;
    std::vector<std::string> injected_libraries;
};

struct target_config {
    bool enabled;
    std::string app_name;
    uint64_t start_up_delay_ms;
    std::vector<std::string> injected_libraries;
    std::vector<int> injected_library_fds;
    child_gating_config child_gating;
};

std::optional<target_config> load_config(int module_dir_fd, std::string const& app_name);
std::optional<target_config> parse_advanced_config(std::string const& config, std::string const& app_name);

#endif  // ZYGISKFRIDA_CONFIG_H
