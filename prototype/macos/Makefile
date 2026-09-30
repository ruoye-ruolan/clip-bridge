PYTHON ?= python3
SWIFTC ?= swiftc
BUILD := build

.PHONY: build test test-python test-swift run clean

build: $(BUILD)/clipboard-watch $(BUILD)/clipboard-path

$(BUILD)/.directory:
	mkdir -p $(BUILD)
	touch $@

$(BUILD)/clipboard-watch: swift/clipboard-watch.swift | $(BUILD)/.directory
	$(SWIFTC) $< -o $@

$(BUILD)/clipboard-path: swift/clipboard-path.swift | $(BUILD)/.directory
	$(SWIFTC) $< -o $@

$(BUILD)/clipboard-watch-test: tests/clipboard-watch-test.swift | $(BUILD)/.directory
	$(SWIFTC) $< -o $@

test: test-python test-swift

test-python:
	$(PYTHON) -m unittest discover -s tests -v

test-swift: build $(BUILD)/clipboard-watch-test
	./$(BUILD)/clipboard-watch-test ./$(BUILD)/clipboard-watch

run: build
	$(PYTHON) -u auto_upload.py --config config.json

clean:
	rm -rf $(BUILD)
