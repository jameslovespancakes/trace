package each

func Each(xs []int, fn func(int)) {
	for _, x := range xs {
		fn(x)
	}
}

type Bus struct {
	handlers []func(string)
}

func (b *Bus) On(h func(string)) {
	b.handlers = append(b.handlers, h)
}

func (b *Bus) Emit(topic string) {
	for _, h := range b.handlers {
		h(topic)
	}
}
