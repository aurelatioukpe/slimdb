# SlimDB C ABI — build the staticlib and the example.
#
#   make            build + run the example
#   make demo       compile only
#   make clean

CC      ?= cc
CARGO   ?= cargo
STATIC  := ../../target/release/libslimdb_c.a
EXAMPLE := examples/demo

.PHONY: all demo clean test

all: test

$(STATIC):
	$(CARGO) build --release -p slimdb-c

demo: $(STATIC) examples/demo.c include/slimdb.h
	$(CC) -O2 -Wall -Wextra -Iinclude examples/demo.c -o $(EXAMPLE) $(STATIC) -lpthread -ldl -lm

test: demo
	rm -f demo.sdb demo.sdb-wal demo-backup.sdb
	./$(EXAMPLE)

clean:
	rm -f $(EXAMPLE) demo.sdb demo.sdb-wal demo-backup.sdb
