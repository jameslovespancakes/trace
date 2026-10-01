#include "shapes.hpp"

namespace shapes {

int scale(int value) {
    return value * 2;
}

}  // namespace shapes

int shapes::Box::volume() const {
    return shapes::scale(1);
}
