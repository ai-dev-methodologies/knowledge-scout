---
schema: card-v1
id: 1f1da92eed2fb19a
card_key: https://docs.docker.com/build/building/multi-stage
title: Docker 멀티스테이지 빌드로 작은 이미지 만들기
source_url: https://docs.docker.com/build/building/multi-stage
scope: public
access: A
topic: delivery
status: synthesized
evidence_type: official_doc
published_at: null
collected_at: 2026-11-11
sha256: null
source_path: null
learning_record: null
learning_record_state: null
entities:
- docker
techniques:
- multi-stage-build
domains:
- containers
- devops
new_terms: []
problems:
- 도커 이미지 크기 줄이기
- 빌드 도구가 운영 이미지에 남는 문제
tech_keywords:
- docker
- multi-stage-build
- containers
- devops
skip_reason: null
---

## 요약

- Dockerfile에 FROM을 여러 번 쓰면 각 FROM이 새 빌드 단계를 시작하고, 단계마다 다른 베이스 이미지를 쓸 수 있다.
- COPY --from으로 앞 단계의 산출물만 골라 가져오고 나머지는 최종 이미지에 남기지 않는다.
- 별도 빌드 스크립트 없이 Dockerfile 하나와 docker build 한 번이면 되며, 결과는 실행 파일만 든 작은 운영 이미지다.

## 한계

예시가 Go 단일 바이너리 하나라서, 런타임이 필요한 언어의 이미지 구성은 직접 따져 봐야 한다.
