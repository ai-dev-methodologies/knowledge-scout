---
schema: card-v1
id: 05cae033cedcdc82
card_key: https://sqlite.org/wal.html
title: 'SQLite WAL 모드: 읽기와 쓰기를 동시에'
source_url: https://sqlite.org/wal.html
scope: public
access: A
topic: data
status: synthesized
evidence_type: official_doc
published_at: null
collected_at: 2026-11-11
sha256: null
source_path: null
learning_record: null
learning_record_state: null
entities:
- sqlite
techniques:
- write-ahead-logging
domains:
- database
new_terms: []
problems:
- 읽기와 쓰기 동시성 개선
- 롤백 저널 대신 WAL 사용
- WAL 사용 제약
tech_keywords:
- sqlite
- write-ahead-logging
- database
skip_reason: null
---

## 요약

- WAL은 변경을 데이터베이스 파일이 아니라 별도 로그에 먼저 추가하는 방식으로, 롤백 저널보다 동시성이 높다.
- 읽는 쪽이 쓰는 쪽을 막지 않고 쓰는 쪽도 읽는 쪽을 막지 않으며, 체크포인트가 로그 내용을 나중에 본 파일로 옮긴다.
- 모든 프로세스가 같은 호스트에 있어야 하고 네트워크 파일시스템에서는 동작하지 않는다. 체크포인트라는 추가 작업도 생긴다.

## 한계

체크포인트 정책과 wal-index 구현 같은 세부 내용은 옮기지 않았다.
