.PHONY: all build clean test docker-build help

# Config
DOCKERFILE := Dockerfile.windows
OUTPUT_DIR := output
EXE_NAME := peakbot.exe
CONTAINER_BUILDER ?= $(shell command -v podman 2>/dev/null || echo docker)

# Default target
all: build

## build: Cross-compile to Windows exe
build: $(OUTPUT_DIR)/$(EXE_NAME)

$(OUTPUT_DIR)/$(EXE_NAME): Dockerfile.windows
	@mkdir -p $(OUTPUT_DIR)
	@echo "🔨 Cross-compiling peakbot for Windows..."
	$(CONTAINER_BUILDER) build \
		--output type=local,dest=$(OUTPUT_DIR) \
		-f $(DOCKERFILE) \
		.
	@echo "✅ Built: $(OUTPUT_DIR)/$(EXE_NAME)"
	@ls -lh $(OUTPUT_DIR)/$(EXE_NAME)

## clean: Remove output artifacts
clean:
	rm -rf $(OUTPUT_DIR)
	@echo "🧹 Cleaned $(OUTPUT_DIR)/"

## docker-build: Alias for build
docker-build: build

## rebuild: Clean and rebuild from scratch
rebuild: clean build

## help: Show this help
help:
	@echo "PeakBot Windows Cross-Compile Makefile"
	@echo ""
	@echo "Usage:"
	@echo "  make [target]"
	@echo ""
	@echo "Targets:"
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+.*?## / {printf "  %-12s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@echo ""
	@echo "Environment:"
	@echo "  CONTAINER_BUILDER  Container runtime to use (default: podman or docker)"
