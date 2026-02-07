from __future__ import annotations

import uuid
from typing import Dict, List, Optional

from pydantic import BaseModel, Field


class VectorMeta(BaseModel):
    meta: Dict[str, str]


class VectorInput(BaseModel):
    id: Optional[uuid.UUID] = None
    vector: List[float]
    meta: Optional[Dict[str, str]] = None
    namespace: Optional[str] = None


class VectorSearchRequest(BaseModel):
    query: List[float]
    k: int = 10
    metric: str = "l2"
    ef_search: Optional[int] = None
    filter_ids: Optional[List[uuid.UUID]] = None
    filter_meta: Optional[Dict[str, str]] = None
    namespace: Optional[str] = None


class VectorSearchHit(BaseModel):
    id: uuid.UUID
    score: float


class DocInput(BaseModel):
    id: Optional[uuid.UUID] = None
    data: Dict
    namespace: Optional[str] = None
    collection: Optional[str] = None


class RowInput(BaseModel):
    id: Optional[uuid.UUID] = None
    data: Dict
    namespace: Optional[str] = None
    table: Optional[str] = None


class QueryInput(BaseModel):
    filter: Dict
    limit: int = 100
    offset: int = 0
    namespace: Optional[str] = None
    collection: Optional[str] = None
    table: Optional[str] = None
