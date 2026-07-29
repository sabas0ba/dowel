#include "core.h"
#include <stdio.h>

#ifdef APP_JSON
/* `json` は core の非公開依存である。core 自身からは見えていること。 */
#include <json/json.h>
#ifndef JSON_API
#error "the private dependency did not reach the target that declared it"
#endif
#endif

static char buf[64];

const char *core_config(void)
{
    snprintf(buf, sizeof buf, "opt=%d fast=%d simd=%d trace=%d json=%d", APP_OPT, CORE_FAST,
             CORE_SIMD, CORE_TRACE, CORE_JSON);
    return buf;
}
