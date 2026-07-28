#include "core.h"
#include <string.h>

/*
 * 実行時の1行と、コンパイル時に見えている識別子が一致すること。
 * 期待値はハーネスに持たせず、同じ構成から2通りに導いて突き合わせる。
 */
int main(void)
{
    const char *s = core_config();

#if CORE_FAST
    if (strstr(s, "fast=1") == NULL) return 1;
    /* 連鎖の帰結。`fast` だけを渡しても `simd` は有効になる。 */
    if (strstr(s, "simd=1") == NULL) return 2;
#else
    if (strstr(s, "fast=0") == NULL) return 3;
#endif

#if CORE_TRACE
    if (strstr(s, "trace=1") == NULL) return 4;
#else
    if (strstr(s, "trace=0") == NULL) return 5;
#endif

#if CORE_JSON
    if (strstr(s, "json=1") == NULL) return 6;
#else
    if (strstr(s, "json=0") == NULL) return 7;
#endif

    return 0;
}
