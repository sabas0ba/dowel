/* 終了状態 0 が成功。テストハーネスは持たない。 */
#include <string.h>

#include "greet.h"

int main(void) { return strlen(greet_message()) > 0 ? 0 : 1; }
