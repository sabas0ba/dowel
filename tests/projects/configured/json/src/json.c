#include <json/json.h>

#ifndef JSON_API
#error "the package does not see its own public define"
#endif

int json_encode(int value)
{
    return value * 2;
}
