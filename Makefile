PREFIX ?= /usr/local
SBINDIR ?= $(PREFIX)/sbin
SYSTEMD_UNIT_DIR ?= /etc/systemd/system
NETWORKD_LINK_DIR ?= /etc/systemd/network
DESTDIR ?=

INSTALL ?= install
CARGO ?= cargo
SH ?= sh
SYSTEMCTL ?= systemctl
UDEVADM ?= udevadm

HELPER_NAME := i40e-postlink
SHELL_HELPER := i40e-postlink
RUST_BINARY := target/release/i40e-postlink
PARITY_SCRIPT := tests/compare-dry-run.sh
SERVICE_FILE := i40e-postlink@.service
LINK_FILES ?= 10-i40e-pf0.link 10-i40e-pf1.link
PF_INTERFACES ?= enp2s0f0np0 enp2s0f1np1

.DEFAULT_GOAL := build

.PHONY: \
	help \
	build \
	build-all \
	build-rust \
	build-shell \
	test \
	test-rust \
	test-shell \
	parity \
	clean \
	install \
	install-links \
	install-service \
	install-common \
	install-rust \
	install-shell \
	activate \
	install-live \
	install-rust-live \
	install-shell-live

help:
	@printf '%s\n' \
	  'Targets:' \
	  '  build              Build the Rust release binary (default goal)' \
	  '  build-all          Build Rust and validate the shell helper' \
	  '  build-rust         Build the Rust release binary' \
	  '  build-shell        Validate shell helper syntax' \
	  '  test               Run Rust tests and shell syntax checks' \
	  '  parity             Run the fixture dry-run parity check' \
	  '  install            Install the Rust helper, unit, and .link files' \
	  '  install-rust       Install the Rust helper, unit, and .link files' \
	  '  install-shell      Install the shell helper, unit, and .link files' \
	  '  activate           Reload systemd and re-trigger PF interfaces' \
	  '  install-live       install + activate for the Rust helper' \
	  '  install-rust-live  install-rust + activate' \
	  '  install-shell-live install-shell + activate' \
	  '' \
	  'Variables:' \
	  '  PF_INTERFACES      Space-separated PF interfaces to trigger/enable' \
	  '  LINK_FILES         .link files copied during install' \
	  '  PREFIX             Install prefix for the helper binary' \
	  '  DESTDIR            Staging root for packaging installs'

build: build-rust

build-all: build-rust build-shell

build-rust:
	$(CARGO) build --release

build-shell:
	$(SH) -n $(SHELL_HELPER)

test: test-rust test-shell

test-rust:
	$(CARGO) test

test-shell:
	$(SH) -n $(SHELL_HELPER)
	$(SH) -n $(PARITY_SCRIPT)

parity: build-rust test-shell
	$(SH) $(PARITY_SCRIPT) --fixture

clean:
	$(CARGO) clean

install: install-rust

install-links:
	$(INSTALL) -d "$(DESTDIR)$(NETWORKD_LINK_DIR)"
	for file in $(LINK_FILES); do \
		$(INSTALL) -m 0644 "$$file" "$(DESTDIR)$(NETWORKD_LINK_DIR)/$$file"; \
	done

install-service:
	$(INSTALL) -d "$(DESTDIR)$(SYSTEMD_UNIT_DIR)"
	$(INSTALL) -m 0644 "$(SERVICE_FILE)" "$(DESTDIR)$(SYSTEMD_UNIT_DIR)/$(SERVICE_FILE)"

install-common: install-links install-service

install-rust: install-common
	@test -x "$(RUST_BINARY)" || { \
		echo "missing $(RUST_BINARY); run 'make build-rust' first"; \
		exit 2; \
	}
	$(INSTALL) -d "$(DESTDIR)$(SBINDIR)"
	$(INSTALL) -m 0755 "$(RUST_BINARY)" "$(DESTDIR)$(SBINDIR)/$(HELPER_NAME)"

install-shell: install-common
	$(INSTALL) -d "$(DESTDIR)$(SBINDIR)"
	$(INSTALL) -m 0755 "$(SHELL_HELPER)" "$(DESTDIR)$(SBINDIR)/$(HELPER_NAME)"

activate:
	@if [ -n "$(DESTDIR)" ]; then \
		echo "activate does not support DESTDIR"; \
		exit 2; \
	fi
	$(SYSTEMCTL) daemon-reload
	$(UDEVADM) control --reload
	@if [ -n "$(strip $(PF_INTERFACES))" ]; then \
		for iface in $(PF_INTERFACES); do \
			$(UDEVADM) trigger --action=add "/sys/class/net/$$iface"; \
			$(SYSTEMCTL) enable --now "$(HELPER_NAME)@$$iface.service"; \
		done; \
	fi

install-rust-live: install-rust activate

install-live: install-rust-live

install-shell-live: install-shell activate