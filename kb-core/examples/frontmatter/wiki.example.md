---
type: wiki
title: Transformer (architecture)
description: How the Transformer architecture works — self-attention, positional encodings, encoder/decoder stacks.
tags:
  - transformer
  - llm
  - attention
---

# Transformer (architecture)

The Transformer is a sequence-transduction architecture based solely on
attention, dispensing with recurrence and convolutions entirely.

## Self-attention

Every position attends to every other position in the source/target
sequence, producing a weighted mix of token representations.
