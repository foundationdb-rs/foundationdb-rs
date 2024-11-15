/*
 * CWorkload.h
 *
 * This source file is part of the FoundationDB open source project
 *
 * Copyright 2013-2024 Apple Inc. and the FoundationDB project authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#pragma once
#ifndef C_WORKLOAD_H
#define C_WORKLOAD_H

#include <stdint.h>
#include <stdbool.h>

#ifndef C_EXPORT
#define C_EXPORT
#endif

typedef struct FDB_metrics FDBMetrics;
typedef struct FDB_promise FDBPromise;
typedef struct FDB_database FDBDatabase;
typedef struct FDB_workloadContext FDBWorkloadContext;
typedef struct Guest_workload GuestWorkload;

typedef enum FDBSeverity {
	FDBSeverity_Debug,
	FDBSeverity_Info,
	FDBSeverity_Warn,
	FDBSeverity_WarnAlways,
	FDBSeverity_Error,
} FDBSeverity;

typedef struct FDBStringPair {
	const char* key;
	const char* val;
} FDBStringPair;

typedef struct FDBMetric {
	const char* key;
	const char* fmt;
	double val;
	bool avg;
} FDBMetric;

C_EXPORT void FDBString_free(char* _this);

C_EXPORT void FDBMetrics_reserve(FDBMetrics* _this, int n);
C_EXPORT void FDBMetrics_push(FDBMetrics* _this, FDBMetric val);

C_EXPORT void FDBPromise_send(FDBPromise* _this, bool val);
C_EXPORT void FDBPromise_free(FDBPromise* _this);

C_EXPORT void FDBWorkloadContext_trace(FDBWorkloadContext* _this,
                                       FDBSeverity sev,
                                       const char* name,
                                       const FDBStringPair* details,
                                       int n);
C_EXPORT uint64_t FDBWorkloadContext_getProcessID(FDBWorkloadContext* _this);
C_EXPORT void FDBWorkloadContext_setProcessID(FDBWorkloadContext* _this, uint64_t processID);
C_EXPORT double FDBWorkloadContext_now(FDBWorkloadContext* _this);
C_EXPORT uint32_t FDBWorkloadContext_rnd(FDBWorkloadContext* _this);
C_EXPORT char* FDBWorkloadContext_getOption(FDBWorkloadContext* _this, const char* name, const char* defaultValue);
C_EXPORT int FDBWorkloadContext_clientId(FDBWorkloadContext* _this);
C_EXPORT int FDBWorkloadContext_clientCount(FDBWorkloadContext* _this);
C_EXPORT int64_t FDBWorkloadContext_sharedRandomNumber(FDBWorkloadContext* _this);

typedef struct FDBWorkload {
	GuestWorkload* inner;
	void (*setup)(GuestWorkload* _this, FDBDatabase* db, FDBPromise* done);
	void (*start)(GuestWorkload* _this, FDBDatabase* db, FDBPromise* done);
	void (*check)(GuestWorkload* _this, FDBDatabase* db, FDBPromise* done);
	void (*getMetrics)(GuestWorkload* _this, FDBMetrics* out);
	double (*getCheckTimeout)(GuestWorkload* _this);
	void (*free)(GuestWorkload* _this);
} FDBWorkload;

#endif
