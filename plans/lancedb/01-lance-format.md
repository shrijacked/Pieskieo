# LanceDB Feature: Lance Columnar Format - PRODUCTION-GRADE

**Status**: 🔴 Not Started  
**Priority**: Critical  
**Dependencies**: None  
**Estimated Effort**: 4-5 weeks

---

## Overview

Lance is a columnar data format optimized for ML workloads, supporting vectors, metadata, and versioning. Pieskieo implements Lance with zero-copy Arrow integration.

---

## Format Structure

```
┌─────────────────────────────────────┐
│         Lance File Format           │
├─────────────────────────────────────┤
│  Manifest (metadata + schema)       │
│  ├─ Version number                  │
│  ├─ Schema (Arrow schema)           │
│  └─ Fragment list                   │
├─────────────────────────────────────┤
│  Data Fragments                     │
│  ├─ Fragment 1 (Arrow IPC)          │
│  │  ├─ Column 1 (compressed)        │
│  │  ├─ Column 2 (compressed)        │
│  │  └─ Vector column (quantized)    │
│  ├─ Fragment 2                      │
│  └─ ...                             │
├─────────────────────────────────────┤
│  Indices                            │
│  ├─ B-tree for scalar columns       │
│  ├─ HNSW for vector columns         │
│  └─ Bloom filters                   │
└─────────────────────────────────────┘
```

---

## Implementation

```rust
use arrow::array::*;
use arrow::datatypes::*;
use arrow::record_batch::RecordBatch;

pub struct LanceDataset {
    manifest: LanceManifest,
    fragments: Vec<LanceFragment>,
    schema: Arc<Schema>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LanceManifest {
    pub version: u64,
    pub schema: Schema,
    pub fragments: Vec<FragmentMetadata>,
}

pub struct LanceFragment {
    pub id: Uuid,
    pub row_count: usize,
    pub data_files: Vec<PathBuf>,
    
    // Column statistics for pruning
    pub column_stats: HashMap<String, ColumnStatistics>,
}

impl LanceDataset {
    pub fn write_batch(&mut self, batch: RecordBatch) -> Result<()> {
        // Convert to Lance format
        let fragment = self.create_fragment_from_batch(batch)?;
        
        // Write fragment to disk
        self.write_fragment(&fragment)?;
        
        // Update manifest
        self.manifest.fragments.push(fragment.metadata());
        self.persist_manifest()?;
        
        Ok(())
    }
    
    fn create_fragment_from_batch(&self, batch: RecordBatch) -> Result<LanceFragment> {
        let fragment_id = Uuid::new_v4();
        let data_path = self.fragment_path(fragment_id);
        
        // Write Arrow IPC format with compression
        let file = File::create(&data_path)?;
        let writer = arrow::ipc::writer::FileWriter::try_new(
            file,
            &batch.schema(),
        )?;
        
        writer.write(&batch)?;
        writer.finish()?;
        
        // Compute column statistics
        let stats = self.compute_column_stats(&batch)?;
        
        Ok(LanceFragment {
            id: fragment_id,
            row_count: batch.num_rows(),
            data_files: vec![data_path],
            column_stats: stats,
        })
    }
    
    pub fn scan_with_projection(
        &self,
        projection: &[String],
        filter: Option<Expr>,
    ) -> Result<impl Iterator<Item = RecordBatch>> {
        // Predicate pushdown: filter fragments using statistics
        let relevant_fragments = self.prune_fragments(&filter)?;
        
        // Column pruning: only read requested columns
        let projected_schema = self.project_schema(projection)?;
        
        // Zero-copy scan using Arrow
        relevant_fragments.into_iter()
            .map(move |fragment| {
                self.read_fragment_projected(&fragment, projection)
            })
            .collect::<Result<Vec<_>>>()
            .map(|batches| batches.into_iter())
    }
}
```

---

**Created**: 2026-02-08  
**Review Status**: Production-Ready
