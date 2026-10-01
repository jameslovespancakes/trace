class Store:
    def save(self):
        return 1

    def load(self):
        return self.save()


def make_store():
    return Store()
