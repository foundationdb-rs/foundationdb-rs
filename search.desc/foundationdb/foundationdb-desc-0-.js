searchState.loadedDescShard("foundationdb", 0, "FoundationDB Rust Client API\nA custom error that layer developers can use\nRepresents a FoundationDB database\nContains the error value\nThis error represent all errors that can be throwed by …\nThe Standard Error type of FoundationDB\nAlias for <code>Result&lt;..., FdbError&gt;</code>\nA <code>KeySelector</code> identifies a particular key in the database.\nWrapper around the boolean representing whether the …\nContains the success value\n<code>RangeOption</code> represents a query parameters for range scan …\nA reference to the <code>RetryableTransaction</code> has been kept\nA retryable transaction, generated by Database.run\nA trait that must be implemented to use <code>Database::transact</code> …\nA set of options that controls the behavior of …\nIn FoundationDB, a transaction is a mutable snapshot of a …\nA cancelled transaction\nA failed to commit transaction.\nA committed transaction.\nAdds a conflict range to a transaction without performing …\nConfiguration of foundationDB API and Network\nModify the database snapshot represented by transaction to …\nThe beginning of the range.\nInitialize the FoundationDB Client API, this can only be …\nCancels the transaction. All pending or future uses of the …\nModify the database snapshot represented by transaction to …\nModify the database snapshot represented by transaction to …\nRaw foundationdb error code\nAttempts to commit the sets and clears previously applied …\nRetrieves the database version number at which a given …\nCreates a new transaction on the given database.\nCreate a database for the default configuration path\nReturns the default Fdb cluster configuration file path\nDirectory provides a tool for managing related subspaces.\nThe end of the range.\nDefinitions of FDBKeys, used in api version 700 and more.\nCreates a <code>KeySelector</code> that picks the first key greater …\nCreates a <code>KeySelector</code> that picks the first key greater …\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nConverts from a raw foundationDB error code\nCreate a database for the given configuration path\nMost functions in the FoundationDB API are asynchronous, …\nReads a value from the database snapshot represented by …\nReturns a list of public network addresses as strings, one …\nReturns an FDBFuture which will be set to the approximate …\nRetrieve a client-side status information in a JSON format.\nGet the estimated byte size of the key range based on the …\nReturns the underlying <code>FdbError</code>, if any.\nResolves a key selector against the keys in the database …\nReturns a value where 0 indicates that the client is idle …\nMapped Range is an experimental feature introduced in FDB …\nMapped Range is an experimental feature introduced in FDB …\nThe metadata version key <code>\\xff/metadataVersion</code> is a key …\nReads all key-value pairs in the database snapshot …\nGets a list of keys that can split the given range into …\nReads all key-value pairs in the database snapshot …\nReads all key-value pairs in the database snapshot …\nThe transaction obtains a snapshot read version …\nReturns an FDBFuture which will be set to the versionstamp …\nAn idempotent TransactOption\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nIndicates the transaction may have succeeded, though not …\nIndicates the operations in the transactions should be …\nIndicates the transaction has not committed, though in a …\nReturns a the key that serves as the anchor for this …\nCreates a <code>KeySelector</code> that picks the last key less than or …\nCreates a <code>KeySelector</code> that picks the last key less than …\nIf non-zero, indicates the maximum number of key-value …\nDefinitions of MappedKeyValues, used in api version 710 …\nOne of the options::StreamingMode values indicating how …\nCreate a database for the given configuration path if any, …\nConstructs a new KeySelector from the given parameters.\nCreate a database for the given configuration path\ncreate a new custom error\nCreate a new FDBDatabase from a raw pointer. Users are …\nReturns the key offset parameter for this <code>KeySelector</code>\nImplements the recommended retry and backoff behavior for …\nImplements the recommended retry and backoff behavior for …\nGenerated configuration types for use with the various …\nTrue if this is an <code>or_equal</code> <code>KeySelector</code>\nPerform a no-op against FDB to check network thread …\nReset the transaction to its initial state.\nReset the transaction to its initial state.\nReset the transaction to its initial state.\nReset transaction to its initial state.\nReverses the range direction.\nIf true, key-value pairs will be returned in reverse …\nRuns a transactional function against this Database with …\nModify the database snapshot represented by transaction to …\nCalled to set an option an on <code>Database</code>.\nCalled to set an option on an FDBTransaction.\nPass through an option given a code and raw data. Useful …\nSets the snapshot read version used by a transaction.\nIf non-zero, indicates a (soft) cap on the combined number …\nImplementation of the Tenants API. Experimental features …\n<code>transact</code> returns a future which retries on error. It tries …\nImplementation of the official tuple layer typecodes\nA watch’s behavior is relative to the transaction that …\nA Builder with which different versions of the Fdb C API …\nStop the associated <code>NetworkRunner</code> and thread if dropped\nA Builder with which the foundationDB network event loop …\nA foundationDB network event loop runner\nAllow to stop the associated and running <code>NetworkRunner</code>.\nA condition object that can wait for the associated …\nStarts the FoundationDB run loop in a dedicated thread. …\nInitialize the foundationDB API and returns a …\nFinalizes the initialization of the Network and returns a …\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the max api version of the underlying Fdb C API …\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nStart the foundationDB network event loop in the current …\nThe version of run-time behavior the API is requested to …\nSet network options.\nSet the version of run-time behavior the API is requested …\nSignals the event loop invoked by <code>Network::run</code> to …\nWait for the associated <code>NetworkRunner</code> to actually run.\nthe destination directory cannot be a subdirectory of the …\nThrown when the subpath cannot be computed due to length …\ncannot get key for the root of a directory partition\ncannot modify the root directory\nthe destination directory cannot be a subdirectory of the …\nthe root directory cannot be moved\ncannot open subspace in the root of a directory partition\ncannot pack for the root of a directory partition\ncannot specify a prefix in a partition.\ncannot get range for the root of a directory partition\ncannot unpack keys using the root of a directory partition\ntried to create an already existing path.\n<code>Directory</code> represents a subspace of keys in a FoundationDB …\nDirectory does not exists\nThe enumeration holding all possible errors from a …\nA DirectoryLayer defines a new root directory. The node …\nDirectoryOutput represents the different output of a …\nA <code>DirectoryPartition</code> is a DirectorySubspace whose prefix …\nYou can open an <code>DirectoryPartition</code> by using the “…\nprefix is already used\nA <code>DirectorySubspace</code> represents the contents of a …\nUnder classic usage, you will obtain an <code>DirectorySubspace</code>\nthe layer is incompatible.\nmissing path.\nParent does not exists\nmissing directory.\ncannot specify a prefix unless manual prefixes are enabled\nPrefix is not empty\nBad directory version.\nCreates a subdirectory of this Directory located at path …\nCreates or opens the subdirectory of this Directory …\nThe default root directory stores directory layer metadata …\nChecks if the subdirectory of this Directory located at …\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nList the subdirectories of this directory at a given …\nMoves this Directory to the specified newAbsolutePath.\nMoves the subdirectory of this Directory located at …\n<code>move_to</code> the directory from old_path to new_path(both …\nOpens the subdirectory of this Directory located at path.\nRemoves the subdirectory of this Directory located at path …\nRemoves the subdirectory of this Directory located at path …\nAn FdbKey, owned by a FoundationDB Future\nAn slice of keys owned by a FoundationDB future\nAn iterator of keyvalues owned by a foundationDB future\nA row key you can own\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nretrieves the associated key\nAn address owned by a foundationDB future\nA slice of addresses owned by a foundationDB future\nA keyvalue owned by a foundationDB future\nA slice of bytes owned by a foundationDB future\nA keyvalue you can own\nAn slice of keyvalues owned by a foundationDB future\nAn iterator of keyvalues owned by a foundationDB future\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nkey\n<code>true</code> if there is another range after this one\nvalue\nA KeyValue produced by a mapped operation, ownder by a …\nAn FdbMappedValue that you can own.\nAn iterator of mapped keyvalues owned by a foundationDB …\nAn slice of mapped keyvalues owned by a foundationDB …\nRetrieves the beginning of the range\nRetrieves the beginning of the range as a <code>KeySelector</code>\nRetrieves the end of the range\nRetrieves the end of the range as a <code>KeySelector</code>\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nretrieves the associated slice of <code>FdbKeyValue</code>\n<code>true</code> if there is another range after this one\nRetrieves the “parent” key that generated the …\nRetrieves the “parent” value that generated the …\nAllows this transaction to read and modify system keys …\naddend\nvalue with which to perform bitwise and\nvalue to append to the database value\nA JSON Web Token authorized to access data belonging to …\nString identifier used to associated this transaction with …\nAutomatically assign a random 16 byte idempotency id for …\nvalue with which to perform bitwise and\nvalue with which to perform bitwise or\nvalue with which to perform bitwise xor\nprobability expressed as a percentage between 0 and 100\nprobability expressed as a percentage between 0 and 100\nAllows this transaction to bypass storage quota …\nAllows <code>get</code> operations to read from sections of keyspace …\nvalue to check against database value\nvalue to check against database value\nIf set, callbacks from external client libraries can be …\nThe read version will be committed, and usually will be …\nThe transaction, if not self-conflicting, may be committed …\nDisable client buggify\nEnable client buggify - will make requests randomly fail …\nprobability expressed as a percentage between 0 and 100\nprobability expressed as a percentage between 0 and 100\nNumber of client threads to be spawned.  Each cluster will …\nClient directory for temporary files.\npath to cluster file\nValue to compare with\nHexadecimal ID\nOptional transaction name\nString identifier to be used when tracing or profiling …\nPrevents the multi-version client API from being disabled, …\nDisables logging of client statistics, such as sampled …\nPrevents connections through the local client, allowing …\nDisables the multi-version client API and instead uses the …\nDistributed tracer type. Choose from none, log_file, or …\nDeprecated\nEnables debugging feature to perform run loop profiling. …\nDeprecated\nInfrequently used. The client has passed a specific row …\nAsks storage servers for how many bytes a clear key range …\npath to directory containing client libraries\npath to client library\nFail with an error if there is no client matching the …\npath to client library\nIgnore the failure to initialize some of the external …\nAddresses returned by get_addresses_for_key include the …\nThis is a write-only transaction which sets the initial …\nThe default. The client doesn’t know how much of the …\nknob_name=knob_value\nInfrequently used. Transfer data in batches large enough …\nIP:PORT\nMax location cache entries\nThe transaction can read and write to locked databases, …\nEnables tracing for this transaction and logs results to …\nHexadecimal ID\nvalue to check against database value\nvalue in milliseconds of maximum delay\nMax outstanding watches\nReturns <code>true</code> if the error indicates the transaction may …\nInfrequently used. Transfer data in batches sized in …\nvalue to check against database value\nThe next write performed on this transaction will not …\nvalue with which to perform bitwise or\nSpecifies that this transaction should be treated as low …\nSpecifies that this transaction should be treated as …\nAllows this transaction to access the raw key-space when …\nUsed to add a read conflict range\nDeprecated\nThe transaction can read from locked databases.\nUse high read priority for subsequent read requests in …\nUse low read priority for subsequent read requests in this …\nUse normal read priority for subsequent read requests in …\nStorage server should not cache disk blocks needed for …\nStorage server should cache disk blocks needed for …\nAllows this transaction to read system keys (those that …\nReads performed by a transaction will not see any prior …\nThe transaction can retrieve keys that are conflicting …\nRetain temporary external client library copies that are …\nnumber of times to retry\nReturns <code>true</code> if the error indicates the operations in the …\nReturns <code>true</code> if the error indicates the transaction has …\nTransfer data in batches large enough that an individual …\nSets an identifier for server tracing of this transaction. …\nvalue to which to set the transformed key\nvalue to versionstamp and set\nvalue in bytes\nInfrequently used. Transfer data in batches small enough …\nSnapshot read operations will not see the results of …\nSnapshot read operations will not see the results of …\nSnapshot read operations will see the results of writes …\nSnapshot read operations will see the results of writes …\nA byte string of length 16 used to associate the span of …\nBy default, users are not allowed to write to special …\nBy default, the special key space will only allow users to …\nca bundle\nfile path\ncertificates\nfile path\nkey\nfile path\nkey passphrase\nfile path or linker-resolved name\nverification pattern\nString identifier used to associated this transaction with …\ninteger between 0 and 100 expressing the probability a …\nvalue in milliseconds of timeout\nTrace clock source\npath to output directory (or NULL for current working …\nThe identifier that will be part of all trace file names\nFormat of trace files\nInitialize trace files on network setup, determine the …\nvalue of the LogGroup attribute\nmax total size of trace files\nAppend this suffix to partially written log files. When a …\nmax size of a single trace output file\nUse the same base trace file name for all client threads …\nSet a random idempotency id for all transactions. See the …\nAllows <code>get</code> operations to read from sections of keyspace …\nThe read version will be committed, and usually will be …\nDeprecated. Addresses returned by get_addresses_for_key …\nString identifier to be used in the logs when tracing this …\nMaximum length of escaped key and value fields.\nMaximum length of escaped key and value fields.\nvalue in milliseconds of maximum delay\nEnables conflicting key reporting on all transactions, …\nnumber of times to retry\nvalue in bytes\nvalue in milliseconds of timeout\nBy default, operations that are performed on a transaction …\nUse configuration database.\nAllows this transaction to use cached GRV from the …\nThis option should only be used by tools which change the …\nBy default, operations that are performed on a transaction …\nClient intends to consume the entire range and would like …\nUsed to add a write conflict range\nvalue with which to perform bitwise xor\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nDisplay a printable version of bytes\nA <code>FdbTenant</code> represents a named key-space within a database …\nHolds the information about a tenant\nThe FoundationDB API includes function to manage the set …\nCreates a new tenant in the cluster using a transaction …\nCreates a new transaction on the given database and tenant.\nDeletes a tenant from the cluster using a transaction …\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the name of this <code>FdbTenant</code>.\nGet a tenant in the cluster using a transaction created on …\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nLists all tenants in between the range specified. The …\nRuns a transactional function against this Tenant with …\n<code>transact</code> returns a future which retries on error. It tries …\nRepresent a sequence of bytes (i.e. &amp;u8)\nContains the error value\nUUID namespace for Domain Name System (DNS).\nUUID namespace for ISO Object Identifiers (OIDs).\nUUID namespace for Uniform Resource Locators (URLs).\nUUID namespace for X.500 Distinguished Names (DNs).\nContains the success value\nA packing/unpacking error\nAlias for <code>Result&lt;..., tuple::Error&gt;</code>\nRepresents a well-defined region of keyspace in a …\nTracks the depth of a Tuple decoding chain\nA type that can be packed\nA type that can be unpacked\nA Universally Unique Identifier (UUID).\n<code>all</code> returns the Subspace corresponding to all keys in a …\nGet a borrowed <code>Braced</code> formatter.\nReturns a slice of 16 octets containing the value.\nReturns the four field values of the UUID.\nGet a borrowed <code>Hyphenated</code> formatter.\nGet a borrowed <code>Simple</code> formatter.\nReturns a 128bit value containing the value.\nReturns two 64bit values containing the value.\nGet a borrowed <code>Urn</code> formatter.\nGet a <code>Braced</code> formatter.\n<code>bytes</code> returns the literal bytes of the prefix of this …\nReturns the current depth in any recursive tuple …\nA buffer that can be used for <code>encode_...</code> calls, that is …\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nReturns the argument unchanged.\nCreates a UUID using the supplied bytes.\nReturns a new Subspace from the provided bytes.\nCreates a UUID using the supplied bytes in little endian …\nCreates a reference to a UUID from a reference to the …\nCreates a UUID from four field values.\nCreates a UUID from four field values in little-endian …\nCreates a UUID using the supplied bytes.\nCreates a UUID using the supplied bytes in little endian …\nCreates a UUID from a 128bit value.\nCreates a UUID from a 128bit value in little-endian order.\nCreates a UUID from two 64bit values.\nIf the UUID is the correct version (v1, or v6) this will …\nIf the UUID is the correct version (v1, v6, or v7) this …\nReturns the variant of the UUID structure.\nReturns the version of the UUID.\nReturns the version number of the UUID.\nThe directory layer offers subspace indirection, where …\nGet a <code>Hyphenated</code> formatter.\nIncrement the depth by one, this be called when calling …\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nConsumes self and returns the underlying byte value of the …\nConvert into prefix key bytes\nTests if the UUID is max (all ones).\nTests if the UUID is nil (all zeros).\n<code>is_start_of</code> returns true if the provided key starts with …\nThe ‘max UUID’ (all ones).\nThe ‘nil UUID’ (all zeros).\nPack value and returns the packed buffer\nReturns the key encoding the specified Tuple with the …\nPack value into the given buffer\nPack value into the given buffer\nPack value into the given buffer\nPack value into the given buffer\nPack value into the given buffer\nPack value into the given buffer\nPack value and returns the packed buffer\nPack value and returns the packed buffer\nPack value and returns the packed buffer\nPack value and returns the packed buffer\nPack value and returns the packed buffer\nReturns the key encoding the specified Tuple with the …\nParses a <code>Uuid</code> from a string of hexadecimal digits with …\n<code>range</code> returns first and last key of given Subspace\nGet a <code>Simple</code> formatter.\nReturns a new Subspace whose prefix extends this Subspace …\nReturns the bytes of the UUID in little-endian order.\nReturns the four field values of the UUID in little-endian …\nReturns a 128bit little-endian value containing the value.\nParses a <code>Uuid</code> from a string of hexadecimal digits with …\nParses a <code>Uuid</code> from a string of hexadecimal digits with …\nUnpack input\n<code>unpack</code> returns the Tuple encoded by the given key with the …\nGet a <code>Urn</code> formatter.\nRepresents a High Contention Allocator for a given subspace\nReturns a byte string that\nReturns the argument unchanged.\nReturns the argument unchanged.\nCalls <code>U::from(self)</code>.\nCalls <code>U::from(self)</code>.\nConstructs an allocator that will use the input subspace …")