// C++ to Rust bindings

#include "headers/ClientWorkload.h"
#include "headers/fdb_c.h"
#include <iostream>

// Opaque type covering a Rust `Box<dyn RustWorkload>`
struct RustWorkload;

template<typename T>
struct Wrapper {
    T inner;
};

extern "C" RustWorkload* workload_instantiate(const char*, FDBWorkloadContext*);
extern "C" const char* workload_description(RustWorkload*);
extern "C" void workload_setup(RustWorkload*, FDBDatabase*, Wrapper<GenericPromise<bool>>*);
extern "C" void workload_start(RustWorkload*, FDBDatabase*, Wrapper<GenericPromise<bool>>*);
extern "C" void workload_check(RustWorkload*, FDBDatabase*, Wrapper<GenericPromise<bool>>*);
extern "C" void workload_get_metrics(RustWorkload*, void*);
extern "C" double workload_get_check_timeout(RustWorkload*);
extern "C" void workload_drop(RustWorkload*);

class WorkloadTranslater: public FDBWorkload {
    private:
        std::string name;
        RustWorkload* rustWorkload;

    public:
        WorkloadTranslater(const std::string& name): FDBWorkload(), name(name), rustWorkload(NULL) {}
        ~WorkloadTranslater() {
            if (this->rustWorkload) {
                workload_drop(this->rustWorkload);
            }
        }

        virtual std::string description() const override {
            return workload_description(this->rustWorkload);
        }
        virtual bool init(FDBWorkloadContext* context) override {
            auto status = fdb_select_api_version(FDB_API_VERSION);
            if (status != 0) {
                std::cout << "ERROR: " << fdb_get_error(status) << std::endl;
            }
            // std::cout << "fdb_get_max_api_version() = " << fdb_get_max_api_version() << std::endl;
            // std::cout << "fdb_get_client_version() = "  << fdb_get_client_version()  << std::endl;
            this->rustWorkload = workload_instantiate(this->name.c_str(), context);
            return true;
        }
        virtual void setup(FDBDatabase* db, GenericPromise<bool> done) override {
            auto wrapped = new Wrapper<GenericPromise<bool>> { done };
            workload_setup(this->rustWorkload, db, wrapped);
        }
        virtual void start(FDBDatabase* db, GenericPromise<bool> done) override {
            auto wrapped = new Wrapper<GenericPromise<bool>> { done };
            workload_start(this->rustWorkload, db, wrapped);
        }
        virtual void check(FDBDatabase* db, GenericPromise<bool> done) override {
            auto wrapped = new Wrapper<GenericPromise<bool>> { done };
            workload_check(this->rustWorkload, db, wrapped);
        }
        virtual void getMetrics(std::vector<FDBPerfMetric>& out) const override {
            workload_get_metrics(this->rustWorkload, &out);
        }
        virtual double getCheckTimeout() override {
            return workload_get_check_timeout(this->rustWorkload);
        }
};

class RustWorkloadFactory: public FDBWorkloadFactory {
    public:
        RustWorkloadFactory(FDBLogger* logger): FDBWorkloadFactory() {
            logger->trace(FDBSeverity::Info, "RustWorkloadFactory", {});
        }

        virtual std::shared_ptr<FDBWorkload> create(const std::string& name) {
            return std::make_shared<WorkloadTranslater>(name);
        }
};

extern "C" FDBWorkloadFactory* CPPWorkloadFactory(FDBLogger* logger) {
    return new RustWorkloadFactory(logger);
}
