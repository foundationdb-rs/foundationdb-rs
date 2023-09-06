// Rust to C++ bindings

#include "headers/ClientWorkload.h"
#include <iostream>
#include <string>

struct CPPStringPair {
	const char* key;
	const char* value;
};

struct CPPMetric {
	const char* name;
	double value;
	bool averaged;
	const char* format_code;
};

template<typename T>
struct Wrapper {
	T inner;
};

extern "C" void FDBContext_trace(FDBWorkloadContext* context, FDBSeverity severity, const char* name, CPPStringPair* pairs, uint32_t n) {
	std::vector<std::pair<std::string, std::string>> details;
	for (uint32_t i = 0 ; i < n ; i++) {
		details.push_back(std::pair<std::string, std::string>(pairs[i].key, pairs[i].value));
	}
	return context->trace(severity, name, details);
}

extern "C" uint64_t FDBContext_getProcessID(FDBWorkloadContext* context) {
	return context->getProcessID();
}
extern "C" void FDBContext_setProcessID(FDBWorkloadContext* context, uint64_t processID) {
	return context->setProcessID(processID);
}
extern "C" double FDBContext_now(FDBWorkloadContext* context) {
	return context->now();
}
extern "C" uint32_t FDBContext_rnd(FDBWorkloadContext* context) {
	return context->rnd();
}
extern "C" const char* FDBContext_getOption(FDBWorkloadContext* context, const char* name, const char* defaultValue, std::string** tmpString) {
	std::string value = context->getOption(name, std::string(defaultValue));
	*tmpString = new std::string(value);
	return (*tmpString)->c_str();
}
extern "C" int FDBContext_clientId(FDBWorkloadContext* context) {
	return context->clientId();
}
extern "C" int FDBContext_clientCount(FDBWorkloadContext* context) {
	return context->clientCount();
}
extern "C" int64_t FDBContext_sharedRandomNumber(FDBWorkloadContext* context) {
	return context->sharedRandomNumber();
}

extern "C" void FDBPromise_send(Wrapper<GenericPromise<bool>>* promise, bool value) {
	promise->inner.send(value);
}
extern "C" void FDBPromise_free(Wrapper<GenericPromise<bool>>* promise) {
	delete promise;
}
extern "C" void FDBString_free(std::string* string) {
	delete string;
}

extern "C" void FDBMetrics_extend(std::vector<FDBPerfMetric>* out, const CPPMetric* metrics, uint32_t n) {
	for (uint32_t i = 0 ; i < n ; i++) {
		out->emplace_back(FDBPerfMetric {
			std::string(metrics[i].name),
			metrics[i].value,
			metrics[i].averaged,
			std::string(metrics[i].format_code),
		});
	}
}
