---
type: wiki
title: Transformer (architecture)
language: en
description: How the Transformer works — self-attention, positional encoders, multi-head attention. Foundation of modern LLMs.
tags:
  - transformer
  - llm
---

# Transformer (architecture)

The Transformer is a neural network architecture for sequence transduction,
introduced by Vaswani et al. (2017). It replaces recurrence with self-attention
and became the basis of modern LLMs.

> The primary paper is tracked by the [[LLM]] label — see its Files list for
> the actual PDF (`papers/attention-is-all-you-need.pdf`).

## Key components

- Multi-head self-attention
- Position-wise feed-forward layers
- Positional encoders (no recurrence, no convolution)
