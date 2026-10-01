class Picker:
    def __init__(self):
        self.row = 0

    def set_selection(self, row):
        self.row = row
        return self


def open_picker():
    picker = Picker()
    return picker.set_selection(1)
