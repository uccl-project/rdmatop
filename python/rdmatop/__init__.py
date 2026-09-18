__all__ = ["enable"]


def enable():
    from rdmatop.kineto import enable as enable_kineto

    enable_kineto()
