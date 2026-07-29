#include <json/json.h>

int main(void)
{
    return json_encode(21) == 42 ? 0 : 1;
}
