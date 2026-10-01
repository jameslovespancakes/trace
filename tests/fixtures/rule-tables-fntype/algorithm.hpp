namespace std {

template <class T> class function;

template <class RandomIt, class Compare>
void sort(RandomIt first, RandomIt last, Compare comp);

void at_quick_exit(const std::function<void()> &handler);

template <std::invocable F>
void run_now(F f);

}  // namespace std
