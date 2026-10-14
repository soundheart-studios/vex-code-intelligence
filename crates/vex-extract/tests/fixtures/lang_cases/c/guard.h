#ifndef GUARD_H
#define GUARD_H
#ifdef __cplusplus
extern "C" {
#endif

typedef struct Node {
  struct Node *next;
  int value;
} Node;

enum Color { RED, GREEN = 2, BLUE };

#if defined(FAST)
static int pick(void) { return fast_pick(); }
#elif defined(SLOW)
static int pick(void) { return slow_pick(); }
#else
static int pick(void) { return 0; }
#endif

struct Outer { struct Inner { int x; } inner; } outer_value;

int visit(Node *n) {
  helper(n);
  n->next = 0;
  return compute(n->value);
}
