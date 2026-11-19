import os, sys, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import i18n_work  # noqa: E402


class Registry(unittest.TestCase):
    def test_rows_of_the_rust_registry(self):
        src = '    En "en" false,\n    PtBr "pt-BR" false,\n    Ar "ar" true,\n'
        self.assertEqual(i18n_work.locale_codes(src), ["en", "pt-BR", "ar"])

    def test_real_registry_has_45_locales(self):
        codes = i18n_work.locales()
        self.assertEqual(len(codes), 45)
        self.assertEqual(codes[:2], ["en", "ru"])


if __name__ == "__main__":
    unittest.main()
