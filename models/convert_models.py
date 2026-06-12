#!/usr/bin/env python3
"""Auto-detect and add GRU state I/O to DeepFilterNet ONNX models."""

import sys, os, onnx
from onnx import helper, TensorProto


def list_grus(model):
    return [(n.name, n) for n in model.graph.node if n.op_type == 'GRU']


def modify_model(input_path, output_path, model_name):
    m = onnx.load(input_path)
    grus = list_grus(m)
    if not grus:
        print(f"  No GRUs found, skipping")
        return

    for i, (name, node) in enumerate(grus):
        hidden_size = None
        for attr in node.attribute:
            if attr.name == 'hidden_size':
                hidden_size = attr.i
                break
        if hidden_size is None:
            print(f"  WARNING: hidden_size not found for {name}")
            continue

        state_shape = [1, 1, hidden_size]
        in_name = f"gru_h_in" if i == 0 else f"gru_h{i+1}_in"
        out_name = f"gru_h_out" if i == 0 else f"gru_h{i+1}_out"
        y_h_name = node.output[1]

        # Add model input
        m.graph.input.append(helper.make_tensor_value_info(in_name, TensorProto.FLOAT, state_shape))

        # Wire new input to GRU initial_h (position 5)
        new_inputs = list(node.input)
        new_inputs[5] = in_name
        node.ClearField('input')
        node.input.extend(new_inputs)

        # Add Y_h as model output
        if y_h_name not in {o.name for o in m.graph.output}:
            m.graph.output.append(helper.make_tensor_value_info(y_h_name, TensorProto.FLOAT, state_shape))

        print(f"  {name}: hidden_size={hidden_size} -> {in_name}/{y_h_name}")

    onnx.checker.check_model(m)
    onnx.save(m, output_path)
    print(f"  Saved: {output_path}")


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <input_dir> <output_dir>")
        sys.exit(1)
    input_dir, output_dir = sys.argv[1], sys.argv[2]
    os.makedirs(output_dir, exist_ok=True)

    for name in ['enc', 'erb_dec', 'df_dec']:
        print(f"\n=== {name} ===")
        modify_model(os.path.join(input_dir, f'{name}.onnx'),
                     os.path.join(output_dir, f'{name}.onnx'), name)
    print("\nDone!")


if __name__ == '__main__':
    main()
