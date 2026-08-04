SHELL := /bin/bash
.DEFAULT_GOAL := help
RUST_TOOLCHAIN ?= stable
CARGO ?= ./scripts/cargo-toolchain.sh $(RUST_TOOLCHAIN)

.PHONY: help bootstrap build build-release build-cross build-all dist fmt clippy test check msrv-check run cli linux-check

help:
	@echo "served development targets"
	@echo "  make bootstrap       Install Rust targets and validate cross tools"
	@echo "  make build           Build a native debug binary"
	@echo "  make build-release   Build a native release binary"
	@echo "  make build-cross     Build a release binary for the other host architecture"
	@echo "  make build-all       Build release binaries for both host architectures"
	@echo "  make dist            Package both host architectures under dist/"
	@echo "  make check           Run format, clippy, and all native tests"
	@echo "  make msrv-check      Check all targets with Rust 1.85.0"
	@echo "  make run             Run an isolated development manager"
	@echo "  make cli ARGS=list   Run a client against the development manager"
	@echo "  make linux-check     Run full Linux checks in Docker"

bootstrap:
	@./scripts/dev.sh bootstrap

build:
	@$(CARGO) build --locked

build-release:
	@$(CARGO) build --release --locked

build-cross:
	@./scripts/build-targets.sh cross

build-all:
	@./scripts/build-targets.sh all

dist:
	@./scripts/package-release.sh

fmt:
	@$(CARGO) fmt --all -- --check

clippy:
	@$(CARGO) clippy --all-targets --locked -- -D warnings

test:
	@$(CARGO) test --locked

check: fmt clippy test

msrv-check:
	@./scripts/dev.sh msrv-check

run:
	@./scripts/dev.sh run

cli:
	@./scripts/dev.sh cli $(ARGS)

linux-check:
	@./scripts/dev.sh linux-check
