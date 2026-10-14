"""Every construct the structure view walks."""


def control(a, items):
    if a:
        return 1
    elif a is None:
        pass
    else:
        a = 2
    for i in items:
        while i:
            i -= 1
    try:
        x = 11
    except ValueError:
        x = 2
    finally:
        x = 3
    with open("f") as fh:
        fh.read()

    def inner():
        return 0

    class Local:
        pass

    match a:
        case 1:
            pass
    return x
