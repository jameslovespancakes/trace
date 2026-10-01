#include "util.hpp"

int main() {
    Greeter g;
    return static_cast<int>(g.greet("trace").size());
}
