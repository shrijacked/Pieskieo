import uuid
from dataclasses import dataclass
from typing import Dict, List, Optional

import httpx


@dataclass
class VectorHit:
    id: uuid.UUID
    score: float


class AsyncPieskieoClient:
    def __init__(self, base_url: str, timeout: float = 5.0):
        self.base = base_url.rstrip("/")
        self.timeout = timeout
        self.client = httpx.AsyncClient(timeout=timeout)

    # -------- vector ops --------
    async def put_vector(
        self,
        vector: List[float],
        id: Optional[uuid.UUID] = None,
        meta: Optional[Dict[str, str]] = None,
        namespace: Optional[str] = None,
    ) -> uuid.UUID:
        vec_id = id or uuid.uuid4()
        payload = {"id": str(vec_id), "vector": vector}
        if namespace:
            payload["namespace"] = namespace
        if meta is not None:
            payload["meta"] = meta
        r = await self.client.post(f"{self.base}/v1/vector", json=payload)
        r.raise_for_status()
        return vec_id

    async def put_vectors_bulk(self, items: List[Dict]) -> int:
        normalized = []
        for it in items:
            vid = it.get("id") or str(uuid.uuid4())
            norm = {"id": vid, "vector": it["vector"], "meta": it.get("meta")}
            if it.get("namespace"):
                norm["namespace"] = it["namespace"]
            normalized.append(norm)
        r = await self.client.post(f"{self.base}/v1/vector/bulk", json={"items": normalized})
        r.raise_for_status()
        return r.json()["data"]

    async def search(
        self,
        query: List[float],
        k: int = 10,
        metric: str = "l2",
        ef_search: Optional[int] = None,
        filter_ids=None,
        filter_meta=None,
        namespace: Optional[str] = None,
    ) -> List[VectorHit]:
        payload = {"query": query, "k": k, "metric": metric}
        if namespace:
            payload["namespace"] = namespace
        if ef_search is not None:
            payload["ef_search"] = ef_search
        if filter_ids:
            payload["filter_ids"] = [str(x) for x in filter_ids]
        if filter_meta:
            payload["filter_meta"] = filter_meta
        r = await self.client.post(f"{self.base}/v1/vector/search", json=payload)
        r.raise_for_status()
        data = r.json()["data"]
        return [VectorHit(id=uuid.UUID(h["id"]), score=h["score"]) for h in data]

    async def delete_vector(self, id: uuid.UUID):
        r = await self.client.delete(f"{self.base}/v1/vector/{id}")
        r.raise_for_status()

    async def get_vector(self, id: uuid.UUID):
        r = await self.client.get(f"{self.base}/v1/vector/{id}")
        r.raise_for_status()
        return r.json()["data"]

    async def update_meta(self, id: uuid.UUID, meta: Dict[str, str]):
        r = await self.client.post(f"{self.base}/v1/vector/{id}/meta", json={"meta": meta})
        r.raise_for_status()

    async def delete_meta_keys(self, id: uuid.UUID, keys: List[str]):
        r = await self.client.post(f"{self.base}/v1/vector/{id}/meta/delete", json={"keys": keys})
        r.raise_for_status()

    # -------- docs / rows --------
    async def put_doc(
        self,
        data,
        id: Optional[uuid.UUID] = None,
        namespace: Optional[str] = None,
        collection: Optional[str] = None,
    ) -> uuid.UUID:
        doc_id = id or uuid.uuid4()
        payload = {"id": str(doc_id), "data": data}
        if namespace:
            payload["namespace"] = namespace
        if collection:
            payload["collection"] = collection
        r = await self.client.post(f"{self.base}/v1/doc", json=payload)
        r.raise_for_status()
        return doc_id

    async def get_doc(
        self,
        id: uuid.UUID,
        namespace: Optional[str] = None,
        collection: Optional[str] = None,
    ):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if collection:
            params["collection"] = collection
        r = await self.client.get(f"{self.base}/v1/doc/{id}", params=params or None)
        r.raise_for_status()
        return r.json()["data"]

    async def delete_doc(
        self,
        id: uuid.UUID,
        namespace: Optional[str] = None,
        collection: Optional[str] = None,
    ):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if collection:
            params["collection"] = collection
        r = await self.client.delete(f"{self.base}/v1/doc/{id}", params=params or None)
        r.raise_for_status()

    # -------- rows --------
    async def put_row(
        self,
        data,
        id: Optional[uuid.UUID] = None,
        namespace: Optional[str] = None,
        table: Optional[str] = None,
    ) -> uuid.UUID:
        row_id = id or uuid.uuid4()
        payload = {"id": str(row_id), "data": data}
        if namespace:
            payload["namespace"] = namespace
        if table:
            payload["table"] = table
        r = await self.client.post(f"{self.base}/v1/row", json=payload)
        r.raise_for_status()
        return row_id

    async def get_row(
        self,
        id: uuid.UUID,
        namespace: Optional[str] = None,
        table: Optional[str] = None,
    ):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if table:
            params["table"] = table
        r = await self.client.get(f"{self.base}/v1/row/{id}", params=params or None)
        r.raise_for_status()
        return r.json()["data"]

    async def delete_row(
        self,
        id: uuid.UUID,
        namespace: Optional[str] = None,
        table: Optional[str] = None,
    ):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if table:
            params["table"] = table
        r = await self.client.delete(f"{self.base}/v1/row/{id}", params=params or None)
        r.raise_for_status()

    async def query_docs(
        self,
        filter: Dict,
        limit: int = 100,
        offset: int = 0,
        namespace: Optional[str] = None,
        collection: Optional[str] = None,
        sql: Optional[str] = None,
    ) -> List:
        if sql:
            r = await self.client.post(f"{self.base}/v1/doc/query", json={"sql": sql})
            r.raise_for_status()
            return r.json()["data"]
        payload = {"filter": filter, "limit": limit, "offset": offset}
        if namespace:
            payload["namespace"] = namespace
        if collection:
            payload["collection"] = collection
        r = await self.client.post(f"{self.base}/v1/doc/query", json=payload)
        r.raise_for_status()
        return r.json()["data"]

    async def query_rows(
        self,
        filter: Dict,
        limit: int = 100,
        offset: int = 0,
        namespace: Optional[str] = None,
        table: Optional[str] = None,
        sql: Optional[str] = None,
    ) -> List:
        if sql:
            r = await self.client.post(f"{self.base}/v1/row/query", json={"sql": sql})
            r.raise_for_status()
            return r.json()["data"]
        payload = {"filter": filter, "limit": limit, "offset": offset}
        if namespace:
            payload["namespace"] = namespace
        if table:
            payload["table"] = table
        r = await self.client.post(f"{self.base}/v1/row/query", json=payload)
        r.raise_for_status()
        return r.json()["data"]

    async def close(self):
        await self.client.aclose()

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        await self.close()
