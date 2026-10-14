#include "guard.h"
#include <stdio.h>

static void (*handler)(int);

int main(int argc, char **argv) {
  printf("%d\n", visit(0));
  handler(1);
  return pick();
}
