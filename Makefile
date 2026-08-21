SHELL := /bin/bash
.DEFAULT_GOAL := help
RUST_TOOLCHAIN ?= stable
CARGO ?= ./scripts/cargo-toolchain.sh $(RUST_TOOLCHAIN)

.PHONY: help bootstrap build build-release build-cross build-all dist source-dist fmt clippy test check shellcheck installer-check systemd-check launchd-check msrv-check run cli linux-check

help:
	@echo "served development targets"
	@echo "  make bootstrap       Install Rust targets and validate cross tools"
	@echo "  make build           Build a native debug binary"
	@echo "  make build-release   Build a native release binary"
	@echo "  make build-cross     Build a release binary for the other host architecture"
	@echo "  make build-all       Build release binaries for both host architectures"
	@echo "  make dist            Package both host architectures under dist/"
	@echo "  make source-dist     Create the deterministic source release archive"
	@echo "  make check           Run format, clippy, and all native tests"
	@echo "  make shellcheck      Check repository shell scripts"
	@echo "  make installer-check Test the online installer without network access"
	@echo "  make systemd-check   Validate the system service template"
	@echo "  make launchd-check   Validate the launchd property list template"
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

source-dist:
	@version="$$(awk -F ' *= *' '$$1 == "version" { gsub(/"/, "", $$2); print $$2; exit }' Cargo.toml)"; \
	./scripts/package-source.sh "dist/served-v$${version}-source.tar.gz" "$${version}"

fmt:
	@$(CARGO) fmt --all -- --check

clippy:
	@$(CARGO) clippy --all-targets --locked -- -D warnings

test:
	@$(CARGO) test --locked

check: fmt clippy test

shellcheck:
	@shellcheck scripts/*.sh tests/*.sh

installer-check:
	@tests/install_online.sh

systemd-check:
	@tests/system_service_template.sh

launchd-check:
	@tests/launchd_template.sh

msrv-check:
	@./scripts/dev.sh msrv-check

run:
	@./scripts/dev.sh run

cli:
	@./scripts/dev.sh cli $(ARGS)

linux-check:
	@./scripts/dev.sh linux-check
