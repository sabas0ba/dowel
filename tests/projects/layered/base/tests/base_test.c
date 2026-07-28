#include <base/base.h>

#ifndef BASE_API
#error "a public define must reach the package's own test target"
#endif

int main(void)
{
    return base_sum(2, 3) == 5 ? 0 : 1;
}
