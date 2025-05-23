// C++ to C bindings

#include <cstring>
#include <cstdint>

#include "headers/CppWorkload.h"
namespace capi {
#include "headers/CWorkload.h"
}

extern "C" capi::FDBWorkload workloadCFactory(const char*, capi::FDBWorkloadContext);

namespace translator {
template <typename T>
struct Wrapper {
	T inner;
};

namespace metrics {
void reserve(capi::OpaqueMetrics* c_metrics, int n) {
	auto metrics = (std::vector<FDBPerfMetric>*)c_metrics;
	metrics->reserve(metrics->size() + n);
}
void push(capi::OpaqueMetrics* c_metrics, capi::FDBMetric c_metric) {
	auto metrics = (std::vector<FDBPerfMetric>*)c_metrics;
	auto fmt = c_metric.fmt ? c_metric.fmt : "%.3g";
	auto metric = FDBPerfMetric{
		.name = std::string(c_metric.key),
		.value = c_metric.val,
		.averaged = c_metric.avg,
		.format_code = std::string(fmt),
	};
	metrics->emplace_back(metric);
}
capi::FDBMetrics wrap(std::vector<FDBPerfMetric>* metrics) {
	return capi::FDBMetrics{
		.inner = (capi::OpaqueMetrics*)metrics,
		.reserve = reserve,
		.push = push,
	};
}
} // namespace metrics

namespace promise {
void send(capi::OpaquePromise* c_promise, bool value) {
	auto promise = (Wrapper<GenericPromise<bool>>*)c_promise;
	promise->inner.send(value);
}
void free(capi::OpaquePromise* c_promise) {
	auto promise = (Wrapper<GenericPromise<bool>>*)c_promise;
	delete promise;
}
capi::FDBPromise wrap(GenericPromise<bool> promise) {
	auto wrapped = new Wrapper<GenericPromise<bool>>{ promise };
	return capi::FDBPromise{
		.inner = (capi::OpaquePromise*)wrapped,
		.send = send,
		.free = free,
	};
}
} // namespace promise

namespace context {
void trace(
	capi::OpaqueWorkloadContext* c_context,
	capi::FDBSeverity c_severity,
	const char* name,
	const capi::FDBStringPair* c_details,
	int n
) {
	auto context = (FDBWorkloadContext*)c_context;
	FDBSeverity severity;
	switch (c_severity) {
	case capi::FDBSeverity_Debug:
		severity = FDBSeverity::Debug;
		break;
	case capi::FDBSeverity_Info:
		severity = FDBSeverity::Info;
		break;
	case capi::FDBSeverity_Warn:
		severity = FDBSeverity::Warn;
		break;
	case capi::FDBSeverity_WarnAlways:
		severity = FDBSeverity::WarnAlways;
		break;
	case capi::FDBSeverity_Error:
		severity = FDBSeverity::Error;
		break;
	default:
		severity = FDBSeverity::Error;
		break;
	}
	std::vector<std::pair<std::string, std::string>> details;
	details.reserve(n);
	for (int i = 0; i < n; i++) {
		details.emplace_back(std::pair<std::string, std::string>(c_details[i].key, c_details[i].val));
	}
	context->trace(severity, name, details);
}
uint64_t getProcessID(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->getProcessID();
}
void setProcessID(capi::OpaqueWorkloadContext* c_context, uint64_t processID) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->setProcessID(processID);
}
double now(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->now();
}
uint32_t rnd(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->rnd();
}
capi::FDBString getOption(capi::OpaqueWorkloadContext* c_context, const char* name, const char* defaultValue) {
	auto context = (FDBWorkloadContext*)c_context;
	std::string value = context->getOption(name, std::string(defaultValue));
	size_t len = value.length() + 1;
	char* c_value = (char*)malloc(len);
	memcpy(c_value, value.c_str(), len);
	return capi::FDBString{
		.inner = c_value,
		.free = (void (*)(const char*))free,
	};
}
int clientId(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->clientId();
}
int clientCount(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->clientCount();
}
int64_t sharedRandomNumber(capi::OpaqueWorkloadContext* c_context) {
	auto context = (FDBWorkloadContext*)c_context;
	return context->sharedRandomNumber();
}
capi::FDBWorkloadContext wrap(FDBWorkloadContext* context) {
	return capi::FDBWorkloadContext{
		.inner = (capi::OpaqueWorkloadContext*)context,
		.trace = trace,
		.getProcessID = getProcessID,
		.setProcessID = setProcessID,
		.now = now,
		.rnd = rnd,
		.getOption = getOption,
		.clientId = clientId,
		.clientCount = clientCount,
		.sharedRandomNumber = sharedRandomNumber,
	};
}
} // namespace context

class Workload : public FDBWorkload {
private:
	capi::FDBWorkload workload{};
	std::string name;

public:
	Workload(const std::string& name) : name(name) {}
	virtual ~Workload() {
		if (this->workload.inner) this->workload.free(this->workload.inner);
	}

#if FDB_API_VERSION <= 730
	virtual std::string description() const override {
		return "unreachable";
	}
#endif

	virtual bool init(FDBWorkloadContext* context) override {
		this->workload = workloadCFactory(this->name.c_str(), context::wrap(context));
		return true;
	}
	virtual void setup(FDBDatabase* db, GenericPromise<bool> done) override {
		return this->workload.setup(this->workload.inner, (capi::FDBDatabase*)db, promise::wrap(done));
	}
	virtual void start(FDBDatabase* db, GenericPromise<bool> done) override {
		return this->workload.start(this->workload.inner, (capi::FDBDatabase*)db, promise::wrap(done));
	}
	virtual void check(FDBDatabase* db, GenericPromise<bool> done) override {
		return this->workload.check(this->workload.inner, (capi::FDBDatabase*)db, promise::wrap(done));
	}
	virtual void getMetrics(std::vector<FDBPerfMetric>& out) const override {
		return this->workload.getMetrics(this->workload.inner, metrics::wrap(&out));
	}
	virtual double getCheckTimeout() override {
		return this->workload.getCheckTimeout(this->workload.inner);
	}
};
} // namespace translator

class CppWorkloadFactory: public FDBWorkloadFactory {
public:
	CppWorkloadFactory(FDBLogger* logger): FDBWorkloadFactory() {
		logger->trace(FDBSeverity::Info, "CppWorkloadFactory", {});
	}

	virtual std::shared_ptr<FDBWorkload> create(const std::string& name) {
		return std::make_shared<translator::Workload>(name);
	}
};

extern "C" FDBWorkloadFactory* workloadCppFactory(FDBLogger* logger) {
	return new CppWorkloadFactory(logger);
}
