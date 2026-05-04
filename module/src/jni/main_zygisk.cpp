#include <unistd.h>

#include <optional>
#include <string>
#include <utility>

#include "config.h"
#include "inject.h"
#include "log.h"
#include "zygisk.h"

using zygisk::Api;
using zygisk::AppSpecializeArgs;
using zygisk::ServerSpecializeArgs;

class MyModule : public zygisk::ModuleBase {
 public:
    void onLoad(Api *api, JNIEnv *env) override {
        this->api = api;
        this->env = env;
    }

    void preAppSpecialize(AppSpecializeArgs *args) override {
        if (args->nice_name == nullptr) {
            return;
        }

        const char *raw_app_name = env->GetStringUTFChars(args->nice_name, nullptr);
        if (raw_app_name == nullptr) {
            return;
        }

        this->app_name = raw_app_name;
        this->env->ReleaseStringUTFChars(args->nice_name, raw_app_name);

        int module_dir_fd = this->api->getModuleDir();
        this->target_config = load_config(module_dir_fd, this->app_name);
        if (module_dir_fd >= 0) {
            close(module_dir_fd);
        }
        if (this->target_config.has_value() && this->target_config->enabled) {
            this->api->setOption(zygisk::Option::FORCE_DENYLIST_UNMOUNT);
        }
    }

    void postAppSpecialize(const AppSpecializeArgs *args) override {
        if (!this->target_config.has_value()) {
            this->api->setOption(zygisk::Option::DLCLOSE_MODULE_LIBRARY);
            return;
        }

        if (!check_and_inject(this->app_name, std::move(this->target_config.value()))) {
            this->api->setOption(zygisk::Option::DLCLOSE_MODULE_LIBRARY);
        }
    }

 private:
    Api *api;
    JNIEnv *env;
    std::string app_name;
    std::optional<target_config> target_config;
};

REGISTER_ZYGISK_MODULE(MyModule)
