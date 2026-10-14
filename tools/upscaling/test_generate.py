"""Check that model texture reuse preserves skip connections and storage formats."""
import unittest
from generate import slots


def stage(inputs, outputs, formats=None):
    return dict(inputs=inputs, outputs=outputs,
                formats=formats or ['rgba16f'] * len(outputs))


class TextureReuse(unittest.TestCase):
    def test_skip_connection_and_same_pass_inputs_stay_live(self):
        graph = [stage(['INPUT'], ['skip', 'a']), stage(['a'], ['b']),
                 stage(['b'], ['c']), stage(['skip', 'c'], ['OUTPUT'])]
        names, textures = slots(graph)
        self.assertEqual(names['a'], names['c'])
        self.assertNotEqual(names['skip'], names['c'])
        self.assertNotEqual(names['b'], names['c'])
        self.assertEqual(textures[names['OUTPUT']], ('rgba16f', 2))

    def test_quantized_storage_is_not_reused_as_float(self):
        graph = [stage(['INPUT'], ['a'], ['rgba8']), stage(['a'], ['b']),
                 stage(['b'], ['c']), stage(['c'], ['OUTPUT'])]
        names, _ = slots(graph)
        self.assertNotEqual(names['a'], names['c'])


if __name__ == '__main__':
    unittest.main()
