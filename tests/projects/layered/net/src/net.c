#include <net/net.h>

#include <base/base.h>
#include <util/util.h>

#ifndef UTIL_API
#error "a private dependency must still reach the target's own compilation"
#endif

int net_port(int requested)
{
    return util_clamp(base_sum(requested, 0), 1024, 65535);
}
