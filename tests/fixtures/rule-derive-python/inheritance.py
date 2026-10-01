"""Constructor chains and self slots (rules: stored callback through super().__init__ /
Base.__init__ / **kwargs forwarding; a self slot of an unrelated class is not this object's)."""


class Parameter:
    def __init__(self, param_decls=None, type=None, required=False, default=None, callback=None):
        self.name = param_decls
        self.type = type
        self.required = required
        self.default = default
        self.callback = callback

    def process_value(self, ctx, value):
        if self.callback is not None:
            value = self.callback(ctx, self, value)
        return value


class Option(Parameter):
    def __init__(self, param_decls=None, show_default=None, **attrs):
        super().__init__(param_decls, **attrs)
        self.show_default = show_default


class Argument(Parameter):
    def __init__(self, param_decls, required=None, **attrs):
        Parameter.__init__(self, param_decls, required=required, **attrs)


class Flag(Parameter):
    def __init__(self, decls, callback=None):
        super().__init__(decls, callback=callback)


class Base:
    pass


class Keeper(Base):
    def __init__(self, handler):
        self.handler = handler


class Caller(Base):
    def __init__(self, handler):
        self.handler = handler

    def fire(self):
        self.handler()


class Relay:
    def __init__(self, handler):
        self.handler = handler

    def attach(self, other, handler):
        other.handler = handler
