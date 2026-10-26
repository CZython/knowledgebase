---
type: note
title: API Gateway — notes
language: en
description: Internal design notes — gRPC vs REST, JWT auth, mTLS, rate-limit algorithms (token bucket).
tags:
  - rust
  - http
  - grpc
---

# API Gateway — notes

Working notes for the team's gateway. This folder is the source of truth for
the design. The actual implementation lives in `../rust/gateway/`.

## Decisions

- gRPC for internal, HTTP/REST for external.
- Auth via JWT; see [[Transformer (architecture)]] for the team's attention-based
  rate-limiting spike.

## Open questions

- Do we need mTLS for east-west traffic yet?
- Rate-limit algorithm: token bucket vs sliding window
