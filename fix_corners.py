from PIL import Image

img = Image.open("docs/assets/logo.png").convert("RGBA")
width, height = img.size
bg_color = img.getpixel((width//2, 10))
pixels = img.load()

corner_size = 150
for y in list(range(corner_size)) + list(range(height - corner_size, height)):
    for x in list(range(corner_size)) + list(range(width - corner_size, width)):
        r, g, b, a = pixels[x, y]
        if r > 100 and g > 100 and b > 100:
            pixels[x, y] = bg_color

img.save("docs/assets/logo_fixed.png")
print("Fixed image saved to docs/assets/logo_fixed.png")
