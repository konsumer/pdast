// PD: expr~ <expression>  — C-style math expression (audio rate)
// Full expr~ translation requires parsing PD's expression syntax.
// The generator emits a passthrough and a comment with the original expression.
// For simple cases, manually replace with the equivalent Faust math.
pdobj = _;
