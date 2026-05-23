[← Phase Index](main.md)

## Phase 5 — Partition-Aware Local Execution

**Goal:** Scale beyond RAM on a single machine using partitioned execution and object store.  
**Success metric:** Process a 100GB dataset on a 16GB machine without OOM.

### 5.1 Partition Model

- [ ] Define `Partition` — a subset of a node's chunks, assigned to a location
- [ ] Define `PartitionLocation`: `Memory`, `LocalDisk`, `Remote { uri }`
- [ ] Implement LRU partition cache — evict cold partitions to disk
- [ ] Implement partition stats: `min`, `max`, `null_count`, `bloom_filter`

### 5.2 Partition Pruning

- [ ] At query time, evaluate filter predicates against partition stats
- [ ] Skip partitions where stats prove no matching rows
- [ ] Log pruning decisions for query explain output

### 5.3 Object Store Integration

- [ ] Implement Parquet partition file reader/writer
- [ ] Implement S3/GCS/R2 partition storage backend
- [ ] Implement Delta Lake / Iceberg partition file format support
- [ ] Implement epoch-based dirty detection from file modification timestamps

