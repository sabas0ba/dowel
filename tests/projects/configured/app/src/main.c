#include "core.h"
#include <stdio.h>

/* `json` は core の非公開依存である。core を使う側へ漏れてはならない。 */
#ifdef JSON_API
#error "a private dependency leaked to a dependent"
#endif

int main(void)
{
    printf("%s\n", core_config());
    return 0;
}
