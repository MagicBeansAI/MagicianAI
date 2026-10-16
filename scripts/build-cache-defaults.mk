# Versioned defaults; environment, command-line and optional host settings win.
MAGICIAN_BUILD_CACHE ?= auto
MAGICIAN_NATIVE_CACHE ?= auto
MBX_VERSION := $(if $(filter undefined,$(origin MBX_VERSION)),$(shell python3 scripts/build_cache_defaults.py version),$(MBX_VERSION))
ifeq ($(origin MAGICIAN_BUILD_STORAGE_ROOT),undefined)
MAGICIAN_BUILD_STORAGE_ROOT := $(or $(BUILD_STORAGE_ROOT),$(patsubst %/mbx,%,$(filter %/mbx,$(MBX_CACHE_DIR))),$(shell python3 scripts/build_cache_defaults.py root))
endif
MAGICIAN_BUILD_CACHE_BOOTSTRAP ?= $(shell python3 scripts/build_cache_defaults.py supported)
MBX_CACHE_DIR ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/mbx
MBX_TARGET_ROOT ?= $(MBX_CACHE_DIR)/targets
# mbx derives disk-scaled storage budgets when no host override supplies them.
MBX_GC_INTERVAL ?= 15m
MBX_SCHEDULER_CPUS := $(if $(filter undefined,$(origin MBX_SCHEDULER_CPUS)),$(shell python3 scripts/build_cache_defaults.py cpus),$(MBX_SCHEDULER_CPUS))
MBX_SCHEDULER_MEMORY := $(if $(filter undefined,$(origin MBX_SCHEDULER_MEMORY)),$(shell python3 scripts/build_cache_defaults.py memory),$(MBX_SCHEDULER_MEMORY))
CARGO_BUILD_JOBS ?= $(MBX_SCHEDULER_CPUS)
export CARGO_BUILD_JOBS
MAGICIAN_BUILD_REQUIRED_VOLUME := $(if $(filter undefined,$(origin MAGICIAN_BUILD_REQUIRED_VOLUME)),$(shell python3 scripts/build_cache_defaults.py volume '$(MAGICIAN_BUILD_STORAGE_ROOT)'),$(MAGICIAN_BUILD_REQUIRED_VOLUME))
MAGICIAN_BUILD_TMPDIR ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/tmp
MAGICIAN_GATED_RUSTC ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/tools/rustc-gated
SCCACHE_CONF ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/sccache/config.toml
SCCACHE_DIR ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/sccache/objects
SCCACHE_CACHE_SIZE ?= 2G
SCCACHE_SERVER_UDS ?= $(MAGICIAN_BUILD_STORAGE_ROOT)/sccache/server.sock
