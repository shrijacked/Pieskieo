import uuid
from dataclasses import dataclass
from typing import Dict, List, Optional

import httpx


@dataclass
class VectorHit:
    id: uuid.UUID
    score: float


class PieskieoClient:
    def __init__(self, base_url: str, timeout: float = 5.0):
        self.base = base_url.rstrip("/")
        self.timeout = timeout
        self.client = httpx.Client(timeout=timeout)

    # -------- vector ops --------
    def put_vector(
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
        r = self.client.post(f"{self.base}/v1/vector", json=payload)
        r.raise_for_status()
        return vec_id

    def put_vectors_bulk(self, items: List[Dict]) -> int:
        # items: [{id?, vector, meta?, namespace?}]
        normalized = []
        for it in items:
            vid = it.get("id") or str(uuid.uuid4())
            norm = {"id": vid, "vector": it["vector"], "meta": it.get("meta")}
            if it.get("namespace"):
                norm["namespace"] = it["namespace"]
            normalized.append(norm)
        r = self.client.post(f"{self.base}/v1/vector/bulk", json={"items": normalized})
        r.raise_for_status()
        return r.json()["data"]

    def search(
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
        r = self.client.post(f"{self.base}/v1/vector/search", json=payload)
        r.raise_for_status()
        data = r.json()["data"]
        return [VectorHit(id=uuid.UUID(h["id"]), score=h["score"]) for h in data]

    def delete_vector(self, id: uuid.UUID):
        r = self.client.delete(f"{self.base}/v1/vector/{id}")
        r.raise_for_status()

    def get_vector(self, id: uuid.UUID):
        r = self.client.get(f"{self.base}/v1/vector/{id}")
        r.raise_for_status()
        return r.json()["data"]

    def update_meta(self, id: uuid.UUID, meta: Dict[str, str]):
        r = self.client.post(f"{self.base}/v1/vector/{id}/meta", json={"meta": meta})
        r.raise_for_status()

    def delete_meta_keys(self, id: uuid.UUID, keys: List[str]):
        r = self.client.post(f"{self.base}/v1/vector/{id}/meta/delete", json={"keys": keys})
        r.raise_for_status()

    # -------- docs / rows --------
    def put_doc(
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
        r = self.client.post(f"{self.base}/v1/doc", json=payload)
        r.raise_for_status()
        return doc_id

    def get_doc(self, id: uuid.UUID, namespace: Optional[str] = None, collection: Optional[str] = None):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if collection:
            params["collection"] = collection
        r = self.client.get(f"{self.base}/v1/doc/{id}", params=params or None)
        r.raise_for_status()
        return r.json()["data"]

    def delete_doc(self, id: uuid.UUID, namespace: Optional[str] = None, collection: Optional[str] = None):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if collection:
            params["collection"] = collection
        r = self.client.delete(f"{self.base}/v1/doc/{id}", params=params or None)
        r.raise_for_status()

    # -------- rows --------
    def put_row(
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
        r = self.client.post(f"{self.base}/v1/row", json=payload)
        r.raise_for_status()
        return row_id

    def get_row(self, id: uuid.UUID, namespace: Optional[str] = None, table: Optional[str] = None):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if table:
            params["table"] = table
        r = self.client.get(f"{self.base}/v1/row/{id}", params=params or None)
        r.raise_for_status()
        return r.json()["data"]

    def delete_row(self, id: uuid.UUID, namespace: Optional[str] = None, table: Optional[str] = None):
        params = {}
        if namespace:
            params["namespace"] = namespace
        if table:
            params["table"] = table
        r = self.client.delete(f"{self.base}/v1/row/{id}", params=params or None)
        r.raise_for_status()

    def query_docs(
        self,
        filter: Dict,
        limit: int = 100,
        offset: int = 0,
        namespace: Optional[str] = None,
        collection: Optional[str] = None,
        sql: Optional[str] = None,
    ) -> List:
        if sql:
            return self.query_sql(sql, limit)
        payload = {"filter": filter, "limit": limit, "offset": offset}
        if namespace:
            payload["namespace"] = namespace
        if collection:
            payload["collection"] = collection
        r = self.client.post(f"{self.base}/v1/doc/query", json=payload)
        r.raise_for_status()
        return r.json()["data"]

    def query_rows(
        self,
        filter: Dict,
        limit: int = 100,
        offset: int = 0,
        namespace: Optional[str] = None,
        table: Optional[str] = None,
        sql: Optional[str] = None,
    ) -> List:
        if sql:
            return self.query_sql(sql, limit)
        payload = {"filter": filter, "limit": limit, "offset": offset}
        if namespace:
            payload["namespace"] = namespace
        if table:
            payload["table"] = table
        r = self.client.post(f"{self.base}/v1/row/query", json=payload)
        r.raise_for_status()
        return r.json()["data"]

    def query_sql(self, sql: str, limit: Optional[int] = None) -> List:
        payload = {"sql": sql}
        if limit is not None:
            payload["limit"] = limit
        r = self.client.post(f"{self.base}/v1/sql", json=payload)
        r.raise_for_status()
        return r.json()["data"]

    # -------- graph --------
    def add_edge(self, src: uuid.UUID, dst: uuid.UUID, weight: float = 1.0):
        r = self.client.post(
            f"{self.base}/v1/graph/edge",
            json={"src": str(src), "dst": str(dst), "weight": weight},
        )
        r.raise_for_status()

    def neighbors(self, id: uuid.UUID, limit: int = 100):
        r = self.client.get(f"{self.base}/v1/graph/{id}")
        r.raise_for_status()
        data = r.json()["data"]
        if limit and len(data) > limit:
            return data[:limit]
        return data

    def bfs(self, id: uuid.UUID, limit: int = 100):
        r = self.client.get(f"{self.base}/v1/graph/{id}/bfs")
        r.raise_for_status()
        data = r.json()["data"]
        if limit and len(data) > limit:
            return data[:limit]
        return data

    def dfs(self, id: uuid.UUID, limit: int = 100):
        r = self.client.get(f"{self.base}/v1/graph/{id}/dfs")
        r.raise_for_status()
        data = r.json()["data"]
        if limit and len(data) > limit:
            return data[:limit]
        return data

    def close(self):
        self.client.close()

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
