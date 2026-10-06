---
type: note
title: API Gateway — notes
tags:
  - gateway
  - http
search_terms:
  - token bucket
  - mTLS
---

# API Gateway — notes

Rough working notes on the gateway's rate-limiter and TLS termination.
Not yet cross-linked; may become a wiki page later.

- token bucket: one per upstream, shared pool rejected
- mTLS between gateway and service mesh
