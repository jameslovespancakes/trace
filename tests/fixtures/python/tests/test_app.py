from shop.app import checkout, main


def test_checkout():
    assert checkout([20.0]) == 18.0


def test_main():
    assert main()
