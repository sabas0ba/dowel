#include <util/util.h>

#ifndef UTIL_API
#error "a public define must reach the package's own test target"
#endif

int main(void)
{
    return (util_clamp(9, 0, 5) == 5 && util_clamp(-1, 0, 5) == 0) ? 0 : 1;
}
