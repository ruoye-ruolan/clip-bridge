PYTHON ?= python3
SWIFTC ?= swiftc
BUILD := build

.PHONY: build test test-python test-swift configure doctor run clean

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

configure:
	$(PYTHON) ../clipbridge configure

doctor:
	$(PYTHON) ../clipbridge doctor

run:
	$(PYTHON) ../clipbridge run

clean:
	rm -rf $(BUILD)
