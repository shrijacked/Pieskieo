# LanceDB Feature: Time Travel Queries - PRODUCTION-GRADE

**Status**: 🔴 Not Started  
**Priority**: Medium  
**Dependencies**: Lance format, versioning  
**Estimated Effort**: 2-3 weeks

---

## Overview

Time travel allows querying data as it existed at any point in history. Pieskieo implements MVCC-based versioning with snapshot isolation.

---

## Query Syntax

```python
# Query current data
df = db.open_table("users").to_pandas()

# Query as of specific version
df = db.open_table("users").checkout_version(42).to_pandas()

# Query as of timestamp
df = db.open_table("users").checkout("2024-01-15T10:30:00Z").to_pandas()

# Query version range (for audit)
versions = db.open_table("users").list_versions()
for v in versions:
    print(f"Version {v.number} at {v.timestamp}")
```

---

## Implementation

```rust
pub struct VersionedDataset {
    current_version: u64,
    versions: BTreeMap<u64, VersionMetadata>,
    
    // Copy-on-write fragments
    fragments: HashMap<Uuid, Arc<LanceFragment>>,
}

#[derive(Debug, Clone)]
pub struct VersionMetadata {
    pub version: u64,
    pub timestamp: SystemTime,
    pub parent: Option<u64>,
    pub fragments: Vec<Uuid>,
    pub tombstones: HashSet<Uuid>,  // Deleted rows
}

impl VersionedDataset {
    pub fn snapshot_at_version(&self, version: u64) -> Result<DatasetSnapshot> {
        let metadata = self.versions.get(&version)
            .ok_or_else(|| PieskieoError::VersionNotFound(version))?;
        
        // Collect all fragments visible at this version
        let visible_fragments: Vec<_> = metadata.fragments.iter()
            .map(|frag_id| self.fragments.get(frag_id).unwrap().clone())
            .collect();
        
        Ok(DatasetSnapshot {
            version,
            fragments: visible_fragments,
            tombstones: metadata.tombstones.clone(),
        })
    }
    
    pub fn write_new_version(&mut self, new_data: RecordBatch) -> Result<u64> {
        let new_version = self.current_version + 1;
        
        // Create new fragment
        let fragment = self.create_fragment(new_data)?;
        let fragment_id = fragment.id;
        
        self.fragments.insert(fragment_id, Arc::new(fragment));
        
        // Create version metadata (copy-on-write)
        let mut new_fragments = self.versions[&self.current_version].fragments.clone();
        new_fragments.push(fragment_id);
        
        let metadata = VersionMetadata {
            version: new_version,
            timestamp: SystemTime::now(),
            parent: Some(self.current_version),
            fragments: new_fragments,
            tombstones: HashSet::new(),
        };
        
        self.versions.insert(new_version, metadata);
        self.current_version = new_version;
        
        Ok(new_version)
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
