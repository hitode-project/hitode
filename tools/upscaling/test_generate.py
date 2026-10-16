"""Check that model texture reuse preserves skip connections and storage formats."""
import unittest
from generate import slots
from fuse import arguments, graph
from matrix import convolution


def stage(inputs, outputs, formats=None):
    return dict(inputs=inputs, outputs=outputs,
                formats=formats or ['rgba16f'] * len(outputs))


class TextureReuse(unittest.TestCase):
    def test_matrix_lowering_preserves_column_weights_bias_and_term_order(self):
        weights = ','.join(f'{i}.0h' for i in range(12))
        first = f'MulAdd_0(left, mat3x4<f16>({weights}), vec4<f16>(1.0h,2.0h,3.0h,4.0h))'
        terms, bias = convolution(['right', f'mat3x4<f16>({weights})', first])
        self.assertEqual([value for value, _ in terms], ['left', 'right'])
        self.assertEqual(terms[0][1], [f'{i}.0h' for i in range(12)])
        self.assertEqual(bias, ['1.0h', '2.0h', '3.0h', '4.0h'])

    def test_fused_graph_versions_overwritten_input_before_reusing_its_slot(self):
        stages = graph([stage(['INPUT'], ['a']), stage(['a'], ['b']),
                        stage(['b'], ['a']), stage(['a'], ['OUTPUT'])])
        fused = [stages[0], dict(stages[1], outputs=stages[2]['outputs']), stages[3]]
        names, _ = slots(fused)
        self.assertNotEqual(names[fused[1]['inputs'][0]], names[fused[1]['outputs'][0]])
        self.assertEqual(fused[2]['inputs'], fused[1]['outputs'])

    def test_nested_convolution_calls_keep_all_matrix_arguments(self):
        args, end = arguments('x, mat4(a,b,c), mul(v, m, bias))tail', 0)
        self.assertEqual(args, ['x', 'mat4(a,b,c)', 'mul(v, m, bias)'])
        self.assertEqual('x, mat4(a,b,c), mul(v, m, bias))tail'[end:], 'tail')

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
