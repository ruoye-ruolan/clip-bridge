PYTHON ?= python3

.PHONY: build test package

build:
	$(MAKE) -C src build

test:
	$(MAKE) -C src test PYTHON="$(PYTHON)"

package:
	$(PYTHON) tools/package.py
