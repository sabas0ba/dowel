#include <net/net.h>

#include <base/base.h>

#ifdef UTIL_API
#error "a private dependency must not propagate to dependents"
#endif
#ifdef NET_INTERNAL
#error "a private define must not reach dependents"
#endif
#ifndef NET_API
#error "a public define must reach dependents"
#endif

int main(void)
{
    return (net_port(80) == 1024 && net_port(70000) == 65535) ? 0 : 1;
}
