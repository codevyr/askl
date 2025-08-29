# Usage:
#   make build                 # build all images
#   make askld-build           # build only askld
#   make create-index-build    # build only create-index
#   make push TAG=v0.1 REGISTRY=ghcr.io/your-org
#   make askld-run             # quick local run, maps 80:80 and ./data -> /data

PROJECTS := askld 
#create-index

# Optional overrides:
REGISTRY ?=
TAG ?= latest
BUILD_ARGS ?=
# If REGISTRY is set (e.g., ghcr.io/your-org), images become ghcr.io/your-org/<name>:<tag>
IMAGE_PREFIX := $(if $(REGISTRY),$(REGISTRY)/,)

define build_template
.PHONY: $(1)-build
$(1)-build:
	docker build $(BUILD_ARGS) \
		-f docker/$(1)/Dockerfile \
		-t $(IMAGE_PREFIX)$(1):$(TAG) \
		.

.PHONY: $(1)-push
$(1)-push:
	docker push $(IMAGE_PREFIX)$(1):$(TAG)

.PHONY: $(1)-run
$(1)-run:
	docker run --rm -p 80:80 -v $$(pwd)/data:/data $(IMAGE_PREFIX)$(1):$(TAG)
endef

$(foreach p,$(PROJECTS),$(eval $(call build_template,$(p))))

.PHONY: build push
build: $(addsuffix -build,$(PROJECTS))
push: $(addsuffix -push,$(PROJECTS))
